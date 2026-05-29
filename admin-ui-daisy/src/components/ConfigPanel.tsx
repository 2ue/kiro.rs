import { BadgeInfo, Gauge, Save, Shield, Sparkles, Trash2, Wand2, Zap } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Alert, Button, Card, Collapse, Input, Join, Loading, Select, Tabs, Toggle } from 'react-daisyui'
import { ErrorState, FieldLabel, SectionCard } from '@/components/common'
import {
  emptyRuntimeConfig,
  fieldNeedsMax,
  fieldNeedsTarget,
  normalizeReportedUsage,
  pathPolicy,
  reportedUsageModeDescription,
  toRatio,
  toScale,
  toWhole,
} from '@/lib/runtime-config-defaults'
import { extractErrorMessage } from '@/lib/utils'
import { useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import type {
  CompatProfile,
  ReportedUsageFieldMode,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RuntimeConfig,
} from '@/types/api'

type ConfigTab = 'dispatch' | 'cache' | 'usage' | 'compat'

const configTabs: Array<{ key: ConfigTab; label: string; description: string }> = [
  { key: 'dispatch', label: '调度', description: '限速、冷却、并发、预热、请求压缩' },
  { key: 'cache', label: '高缓存', description: '缓存模拟比例、放大、触顶扣减' },
  { key: 'usage', label: '路径上报', description: '按路径改写 input、output、cache read/write' },
  { key: 'compat', label: '兼容诊断', description: '协议兼容、调试头、后台统计' },
]

function numberValue(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

function NumberField({
  title,
  description,
  value,
  min,
  max,
  step,
  suffix,
  onChange,
}: {
  title: string
  description: string
  value: number
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
    <Card bordered className="bg-base-100 shadow-none">
      <Card.Body className="flex-row items-center justify-between gap-3 p-3">
      <div className="min-w-0">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</div>
      </div>
      <Toggle color="primary" size="sm" className="shrink-0" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
      </Card.Body>
    </Card>
  )
}

function ConfigGroup({
  icon,
  title,
  description,
  children,
}: {
  icon: React.ReactNode
  title: string
  description: string
  children: React.ReactNode
}) {
  return (
    <Collapse icon="arrow" open className="rounded-box border border-base-300 bg-base-100 shadow-none">
      <Collapse.Title className="flex items-start gap-2.5 px-3 py-2.5">
        <span className="rounded-lg border border-base-300 bg-base-200 p-1.5 text-primary">{icon}</span>
        <span>
          <span className="block text-sm font-semibold">{title}</span>
          <span className="mt-0.5 block text-xs leading-4 text-base-content/60">{description}</span>
        </span>
      </Collapse.Title>
      <Collapse.Content>
        <div className="grid gap-3 border-t border-base-300/70 pt-3 md:grid-cols-2">{children}</div>
      </Collapse.Content>
    </Collapse>
  )
}

function ModeSelect({ value, disabled, onChange }: { value: ReportedUsageFieldMode; disabled?: boolean; onChange: (value: ReportedUsageFieldMode) => void }) {
  return (
    <Select bordered size="sm" className="w-full" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value as ReportedUsageFieldMode)}>
      <Select.Option value="raw">原始值（不经过缓存计算）</Select.Option>
      <Select.Option value="preserve">保留计算值（不改写）</Select.Option>
      <Select.Option value="sample-max">按上限采样改写</Select.Option>
      <Select.Option value="sample-target">按目标采样改写</Select.Option>
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
    <Card bordered className="bg-base-100 shadow-none">
      <Card.Body className="p-3">
      <div className="mb-2">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</div>
      </div>
      <div className="space-y-2.5">
        <ModeSelect value={value.mode} disabled={disabled} onChange={(mode) => onChange({ ...value, mode })} />
        <div className="rounded-box bg-base-200 px-2.5 py-1.5 text-xs leading-4 text-base-content/65">{reportedUsageModeDescription(value.mode)}</div>
        {fieldNeedsMax(value) && (
          <PolicyNumberInput
            title="采样上限"
            description="控制改写后的最大 token 数。实际值会在 1 到这个上限之间自然浮动。"
            value={value.maxTokens}
            min={0}
            suffix="tokens"
            disabled={disabled}
            onChange={(maxTokens) => onChange({ ...value, maxTokens })}
          />
        )}
        {fieldNeedsTarget(value) && (
          <div className="grid gap-3 md:grid-cols-2">
            <PolicyNumberInput
              title="目标值"
              description="控制采样分布的目标 token 数。比如 writer 设置 3000，表示常规结果围绕 3000 附近自然浮动。"
              value={value.targetTokens}
              min={0}
              suffix="tokens"
              disabled={disabled}
              onChange={(targetTokens) => onChange({ ...value, targetTokens })}
            />
            <PolicyNumberInput
              title="常规最大倍率"
              description="控制正常随机范围的上限，常规最大值 = 目标值 × 倍率。"
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
            description="开启后，input_tokens 被压低的差值会加到 cache_read_input_tokens，只改变下游上报外观。"
            checked={value.moveDeltaToCacheRead}
            disabled={disabled || value.mode === 'preserve' || value.mode === 'raw'}
            onChange={(moveDeltaToCacheRead) => onChange({ ...value, moveDeltaToCacheRead })}
          />
        )}
      </div>
      </Card.Body>
    </Card>
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
    <Card bordered className="bg-base-200/55 shadow-none">
      <Card.Body className="p-3">
      <div className="mb-3 flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h4 className="text-sm font-semibold">{title}</h4>
          <p className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</p>
        </div>
        <div className="flex shrink-0 items-center justify-between gap-2 sm:justify-end">
          {onDelete && (
            <Button type="button" color="error" variant="outline" size="xs" onClick={onDelete} title="删除这条路径覆盖">
              <Trash2 className="h-3.5 w-3.5" />
              删除覆盖
            </Button>
          )}
          <Toggle color="primary" size="sm" className="shrink-0" checked={value.enabled} onChange={(event) => onChange({ ...value, enabled: event.target.checked })} />
        </div>
      </div>
      {!value.enabled && (
        <Alert status="warning" className="mb-3 py-2 text-xs leading-5">
          当前路径已关闭本地模拟缓存上报：下游响应和后台 usage 记录会隐藏模拟 cache read/write，并把 input 展示为完整输入。字段改写配置已隐藏，重新开启后才会显示并生效。
        </Alert>
      )}
      {value.enabled && (
        <div className="grid gap-3 xl:grid-cols-2">
          <ReportedUsageFieldEditor
            title="输入字段改写（input_tokens）"
            description="控制给下游和后台记录的 input_tokens。原始值表示请求输入是多少就报多少；保留计算值表示使用 high-cache 计算后的 input；采样可把 input 压到几十以内并把差值计入缓存读取。"
            value={value.input}
            allowMoveDelta
            onChange={(input) => onChange({ ...value, input })}
          />
          <ReportedUsageFieldEditor
            title="输出字段改写（output_tokens）"
            description="控制给下游和后台记录的 output_tokens。默认建议使用原始值，避免本地模拟影响客户端对输出量的判断。"
            value={value.output}
            onChange={(output) => onChange({ ...value, output })}
          />
          <ReportedUsageFieldEditor
            title="缓存读取字段改写（cache_read_input_tokens）"
            description="控制计算完成后给下游和后台记录的 cache_read_input_tokens。保留计算值表示保留 high-cache/上游 metadata/估算后的读缓存值。"
            value={value.cacheRead}
            onChange={(cacheRead) => onChange({ ...value, cacheRead })}
          />
          <ReportedUsageFieldEditor
            title="缓存写入字段改写（cache_creation_input_tokens）"
            description="控制计算完成后给下游和后台记录的 cache_creation_input_tokens。/cc 可设置目标值 3000，实际会自然浮动。"
            value={value.cacheCreation}
            onChange={(cacheCreation) => onChange({ ...value, cacheCreation })}
          />
        </div>
      )}
      </Card.Body>
    </Card>
  )
}

export function ConfigPanel() {
  const config = useRuntimeConfig()
  const updateConfig = useUpdateRuntimeConfig()
  const [draft, setDraft] = useState<RuntimeConfig>(emptyRuntimeConfig)
  const [activeTab, setActiveTab] = useState<ConfigTab>('dispatch')

  useEffect(() => {
    if (config.data) setDraft(config.data)
  }, [config.data])

  const save = () => {
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
      payloadGuardMaxBytes: toWhole(draft.payloadGuardMaxBytes, 65536),
      promptCacheTargetReadRatio: toRatio(draft.promptCacheTargetReadRatio),
      promptCacheTokenScale: toScale(draft.promptCacheTokenScale),
      promptCacheMaxSimulatedInputTokens: toWhole(draft.promptCacheMaxSimulatedInputTokens),
      promptCacheCapJitterMinTokens: toWhole(draft.promptCacheCapJitterMinTokens),
      promptCacheCapJitterMaxTokens: toWhole(draft.promptCacheCapJitterMaxTokens),
      promptCacheScaleMinInputTokens: toWhole(draft.promptCacheScaleMinInputTokens),
      reportedUsage: normalizeReportedUsage(draft.reportedUsage),
      highCacheThreshold: toWhole(draft.highCacheThreshold),
    }
    if (next.credentialTransientCooldownSecs > next.credentialMaxCooldownSecs) return toast.error('临时冷却秒数不能大于最大冷却秒数')
    if ([next.credentialRateLimitCooldownSecs, next.credentialServerErrorCooldownSecs, next.credentialNetworkErrorCooldownSecs, next.credentialStreamErrorCooldownSecs, next.credentialProtocolErrorCooldownSecs, next.credentialAuthErrorCooldownSecs].some((value) => value > next.credentialMaxCooldownSecs)) return toast.error('错误类型基础冷却秒数不能大于最大冷却秒数')
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens) return toast.error('触顶扣减下限不能大于上限')
    if (next.payloadGuardEnabled && next.payloadGuardMaxBytes < 65536) return toast.error('Kiro Payload 最大字节数不能小于 65536')
    updateConfig.mutate(next, {
      onSuccess: () => toast.success('配置已更新'),
      onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
    })
  }

  if (config.isLoading) return <div className="py-10 text-center text-base-content/60">加载中...</div>
  if (config.error) return <ErrorState text={extractErrorMessage(config.error)} />

  return (
    <SectionCard
      title="运行时配置"
      description="这些配置会写入 PgSQL 并对后续新请求热加载生效；监听地址、密钥、数据库连接和代理客户端等启动期配置仍需要改启动配置后重启。"
    >
      <div className="space-y-4">
        <div className="sticky top-[4.25rem] z-30 flex flex-col gap-2 rounded-box border border-base-300 bg-base-100/95 p-2 shadow-sm backdrop-blur sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0 text-xs leading-5 text-base-content/60">
            修改运行时配置后点击保存，新请求会热加载生效。
          </div>
          <Button type="button" color="primary" size="sm" className="shrink-0" onClick={save} disabled={updateConfig.isPending}>
            {updateConfig.isPending ? <Loading size="xs" /> : <Save className="h-4 w-4" />}
            保存
          </Button>
        </div>

        <Tabs variant="boxed" size="sm" className="config-tabs">
          {configTabs.map((tab) => (
            <Tabs.Tab
              key={tab.key}
              href="#"
              active={activeTab === tab.key}
              className="config-tab"
              onClick={(event) => {
                event.preventDefault()
                setActiveTab(tab.key)
              }}
            >
              <span className="font-semibold">{tab.label}</span>
              <span className="hidden text-[0.68rem] text-base-content/55 md:block">{tab.description}</span>
            </Tabs.Tab>
          ))}
        </Tabs>

        {activeTab === 'dispatch' && (
          <>
            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="凭据限速与冷却" description="控制单个账号被调用的频率，以及上游临时错误后多久再尝试使用该账号。">
              <NumberField title="单凭据每分钟请求上限" description="控制每个凭据每分钟最多承接多少请求。填 0 表示关闭本地限速。" value={draft.credentialRpm} min={0} suffix="次/分钟" onChange={(credentialRpm) => setDraft((prev) => ({ ...prev, credentialRpm }))} />
              <NumberField title="单凭据最大并发请求数" description="控制同一个凭据同时处理多少个请求。填 0 表示不限制。" value={draft.credentialMaxConcurrentRequests} min={0} suffix="并发" onChange={(credentialMaxConcurrentRequests) => setDraft((prev) => ({ ...prev, credentialMaxConcurrentRequests }))} />
              <NumberField title="兼容默认冷却秒数" description="供旧调用路径使用的默认冷却值。明确分类的错误使用下方独立设置。" value={draft.credentialTransientCooldownSecs} min={1} suffix="秒" onChange={(credentialTransientCooldownSecs) => setDraft((prev) => ({ ...prev, credentialTransientCooldownSecs }))} />
              <NumberField title="429 基础冷却" description="上游没有返回 Retry-After 时，限流错误首次触发的冷却时长。" value={draft.credentialRateLimitCooldownSecs} min={1} suffix="秒" onChange={(credentialRateLimitCooldownSecs) => setDraft((prev) => ({ ...prev, credentialRateLimitCooldownSecs }))} />
              <NumberField title="5xx / 408 基础冷却" description="上游过载或超时响应首次触发的冷却时长。" value={draft.credentialServerErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialServerErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialServerErrorCooldownSecs }))} />
              <NumberField title="网络错误基础冷却" description="发送失败、连接中断等网络错误首次触发的冷却时长。" value={draft.credentialNetworkErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialNetworkErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialNetworkErrorCooldownSecs }))} />
              <NumberField title="流读取错误基础冷却" description="流读取错误或上游 idle timeout 首次触发的冷却时长。" value={draft.credentialStreamErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialStreamErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialStreamErrorCooldownSecs }))} />
              <NumberField title="协议异常基础冷却" description="可重试协议不匹配和未分类瞬态错误首次触发的冷却时长。" value={draft.credentialProtocolErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialProtocolErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialProtocolErrorCooldownSecs }))} />
              <NumberField title="认证判定基础冷却" description="401/403 触发刷新或失败判定期间暂停继续调度该账号的时长。" value={draft.credentialAuthErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialAuthErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialAuthErrorCooldownSecs }))} />
              <NumberField title="连续失败退避倍率" description="同一凭据连续发生瞬态错误时冷却倍增倍率。" value={draft.credentialCooldownBackoffMultiplier} min={1} max={10} step={0.1} suffix="倍" onChange={(credentialCooldownBackoffMultiplier) => setDraft((prev) => ({ ...prev, credentialCooldownBackoffMultiplier }))} />
              <NumberField title="冷却随机抖动" description="对没有 Retry-After 的退避增加随机偏移，降低并发同时恢复。" value={draft.credentialCooldownJitterPercent} min={0} max={100} suffix="%" onChange={(credentialCooldownJitterPercent) => setDraft((prev) => ({ ...prev, credentialCooldownJitterPercent }))} />
              <NumberField title="恢复观察窗口" description="冷却结束后仍降低该凭据的调度权重，成功后逐步恢复。" value={draft.credentialProbationSecs} min={0} suffix="秒" onChange={(credentialProbationSecs) => setDraft((prev) => ({ ...prev, credentialProbationSecs }))} />
              <NumberField title="最大冷却秒数" description="控制单个凭据最长冷却时间。" value={draft.credentialMaxCooldownSecs} min={1} suffix="秒" onChange={(credentialMaxCooldownSecs) => setDraft((prev) => ({ ...prev, credentialMaxCooldownSecs }))} />
              <NumberField title="单请求最长排队等待" description="所有可用凭据都处于冷却、限速或并发占满时最多等待多久。填 0 表示不限制。" value={draft.credentialDispatchMaxWaitSecs} min={0} suffix="秒" onChange={(credentialDispatchMaxWaitSecs) => setDraft((prev) => ({ ...prev, credentialDispatchMaxWaitSecs }))} />
              <NumberField title="异常并发自动回收" description="单个并发占用超过多久未活跃时自动释放。填 0 表示关闭。" value={draft.credentialInFlightLeaseMaxSecs} min={0} suffix="秒" onChange={(credentialInFlightLeaseMaxSecs) => setDraft((prev) => ({ ...prev, credentialInFlightLeaseMaxSecs }))} />
              <NumberField title="全局最大并发请求数" description="控制所有凭据合计可同时处理的请求数。填 0 表示不限制。" value={draft.dispatchGlobalMaxConcurrentRequests} min={0} suffix="并发" onChange={(dispatchGlobalMaxConcurrentRequests) => setDraft((prev) => ({ ...prev, dispatchGlobalMaxConcurrentRequests }))} />
              <NumberField title="最大等待队列请求数" description="调度容量已满时允许排队等待的请求数量。填 0 表示不限制。" value={draft.dispatchMaxQueuedRequests} min={0} suffix="请求" onChange={(dispatchMaxQueuedRequests) => setDraft((prev) => ({ ...prev, dispatchMaxQueuedRequests }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="健康评分调度" description="均衡/健康均衡模式使用共享错误率、延迟与实时并发为候选排序，并在最佳候选中分散请求。">
              <NumberField title="错误 EWMA 新样本权重" description="越高越快响应近期故障，范围 0.01 到 1。" value={draft.schedulerErrorEwmaAlpha} min={0.01} max={1} step={0.01} suffix="系数" onChange={(schedulerErrorEwmaAlpha) => setDraft((prev) => ({ ...prev, schedulerErrorEwmaAlpha }))} />
              <NumberField title="优先级权重" description="配置优先级对健康得分的影响。" value={draft.schedulerPriorityWeight} min={0} step={0.1} suffix="权重" onChange={(schedulerPriorityWeight) => setDraft((prev) => ({ ...prev, schedulerPriorityWeight }))} />
              <NumberField title="实时负载权重" description="当前在途并发对健康得分的影响。" value={draft.schedulerLoadWeight} min={0} step={1} suffix="权重" onChange={(schedulerLoadWeight) => setDraft((prev) => ({ ...prev, schedulerLoadWeight }))} />
              <NumberField title="近期错误率权重" description="近期上游错误率对健康得分的影响。" value={draft.schedulerErrorWeight} min={0} step={1} suffix="权重" onChange={(schedulerErrorWeight) => setDraft((prev) => ({ ...prev, schedulerErrorWeight }))} />
              <NumberField title="耗时权重" description="每毫秒成功耗时 EWMA 对健康得分的影响。" value={draft.schedulerLatencyWeight} min={0} step={0.001} suffix="权重" onChange={(schedulerLatencyWeight) => setDraft((prev) => ({ ...prev, schedulerLatencyWeight }))} />
              <NumberField title="恢复观察惩罚" description="处于观察窗口时额外增加的健康得分。" value={draft.schedulerProbationWeight} min={0} step={1} suffix="权重" onChange={(schedulerProbationWeight) => setDraft((prev) => ({ ...prev, schedulerProbationWeight }))} />
              <NumberField title="近期调度压力权重" description="最近 60 秒被选中比例高于平均值时增加的降权，避免短时间集中打同一账号。" value={draft.schedulerSelectionPressureWeight} min={0} step={1} suffix="权重" onChange={(schedulerSelectionPressureWeight) => setDraft((prev) => ({ ...prev, schedulerSelectionPressureWeight }))} />
              <NumberField title="总调度次数权重" description="总调度次数对健康得分的影响。默认 0，只建议作为很弱的长期均衡信号。" value={draft.schedulerTotalSelectionWeight} min={0} step={0.001} suffix="权重" onChange={(schedulerTotalSelectionWeight) => setDraft((prev) => ({ ...prev, schedulerTotalSelectionWeight }))} />
              <NumberField title="最佳候选抽样数量" description="从得分最佳的前 N 个账号按权重选择，降低请求集中。" value={draft.schedulerTopK} min={1} max={100} suffix="个" onChange={(schedulerTopK) => setDraft((prev) => ({ ...prev, schedulerTopK }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Sparkles className="h-4 w-4" />} title="新凭据预热" description="预热不会伪造成功次数；批量导入时按预热账号数量分配目标流量，避免新账号长期吃不到请求。">
              <NumberField title="预热剩余请求数" description="新添加凭据默认进入预热状态的请求次数。填 0 表示不预热。" value={draft.credentialWarmupRequests} min={0} suffix="次" onChange={(credentialWarmupRequests) => setDraft((prev) => ({ ...prev, credentialWarmupRequests }))} />
              <NumberField title="预热凭据参与概率" description="每个预热凭据的目标参与比例。批量导入时会按预热账号数放大。" value={draft.credentialWarmupSelectionPercent} min={0} max={100} suffix="%" onChange={(credentialWarmupSelectionPercent) => setDraft((prev) => ({ ...prev, credentialWarmupSelectionPercent }))} />
              <NumberField title="预热总流量上限" description="已有非预热账号可用时，所有预热账号合计最多承接的真实请求比例。" value={draft.credentialWarmupMaxSelectionPercent} min={0} max={100} suffix="%" onChange={(credentialWarmupMaxSelectionPercent) => setDraft((prev) => ({ ...prev, credentialWarmupMaxSelectionPercent }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Wand2 className="h-4 w-4" />} title="请求压缩" description="控制发往上游前是否压缩请求内容。默认关闭总开关；如需开启，建议只使用空白压缩。">
              <ToggleField title="启用请求压缩" description="控制是否对上游请求做压缩处理。关闭时不会改变请求内容。" checked={draft.compressionEnabled} onChange={(compressionEnabled) => setDraft((prev) => ({ ...prev, compressionEnabled }))} />
              <ToggleField title="仅压缩空白字符" description="控制压缩时是否只处理多余空白。这是当前推荐的低风险压缩方式。" checked={draft.whitespaceCompression} disabled={!draft.compressionEnabled} onChange={(whitespaceCompression) => setDraft((prev) => ({ ...prev, whitespaceCompression }))} />
              <ToggleField title="启用 Kiro Payload 防护" description="发送上游前按真实 JSON 字节数检查请求，并修复空 toolUses、孤立 tool_result 等 Kiro 容易拒绝的形态。" checked={draft.payloadGuardEnabled} onChange={(payloadGuardEnabled) => setDraft((prev) => ({ ...prev, payloadGuardEnabled }))} />
              <ToggleField title="超限裁剪旧历史" description="请求超过最大字节数时，优先裁剪最旧历史；关闭后只做协议修复，仍超限会直接返回客户端错误。" checked={draft.payloadGuardTrimHistory} disabled={!draft.payloadGuardEnabled} onChange={(payloadGuardTrimHistory) => setDraft((prev) => ({ ...prev, payloadGuardTrimHistory }))} />
              <NumberField title="Kiro Payload 最大字节数" description="按最终发送到 Kiro 的 JSON body 字节数计算。默认 460800 字节，用于提前规避上游 Improperly formed request。" value={draft.payloadGuardMaxBytes} min={65536} suffix="bytes" onChange={(payloadGuardMaxBytes) => setDraft((prev) => ({ ...prev, payloadGuardMaxBytes }))} />
            </ConfigGroup>
          </>
        )}

        {activeTab === 'cache' && (
          <ConfigGroup icon={<Zap className="h-4 w-4" />} title="高缓存模拟" description="控制 /v1/messages 和 /cc/v1/messages 的本地高缓存 usage 模拟。只影响下游看到的统计和后台记录，不影响 count_tokens 计算接口。">
            <NumberField title="缓存读取目标比例" description="cache_read_input_tokens 大致占输入的目标比例。常用值 0.95 到 0.99。" value={draft.promptCacheTargetReadRatio} min={0} max={0.99} step={0.01} suffix="比例" onChange={(promptCacheTargetReadRatio) => setDraft((prev) => ({ ...prev, promptCacheTargetReadRatio }))} />
            <NumberField title="高缓存输入放大倍数" description="控制高缓存模拟时 total input 的放大程度。只影响缓存计算，不代表 input 上报一定放大。" value={draft.promptCacheTokenScale} min={1} max={3} step={0.1} suffix="倍" onChange={(promptCacheTokenScale) => setDraft((prev) => ({ ...prev, promptCacheTokenScale }))} />
            <NumberField title="模拟输入上限" description="高缓存模拟后 total input 的最高值。填 0 表示不设置上限。" value={draft.promptCacheMaxSimulatedInputTokens} min={0} suffix="tokens" onChange={(promptCacheMaxSimulatedInputTokens) => setDraft((prev) => ({ ...prev, promptCacheMaxSimulatedInputTokens }))} />
            <NumberField title="放大启用门槛" description="基础输入达到多少 tokens 后才启用输入放大。" value={draft.promptCacheScaleMinInputTokens} min={0} suffix="tokens" onChange={(promptCacheScaleMinInputTokens) => setDraft((prev) => ({ ...prev, promptCacheScaleMinInputTokens }))} />
            <NumberField title="触顶扣减下限" description="模拟输入达到上限时，最少从上限扣掉多少 tokens。" value={draft.promptCacheCapJitterMinTokens} min={0} suffix="tokens" onChange={(promptCacheCapJitterMinTokens) => setDraft((prev) => ({ ...prev, promptCacheCapJitterMinTokens }))} />
            <NumberField title="触顶扣减上限" description="模拟输入达到上限时，最多从上限扣掉多少 tokens。" value={draft.promptCacheCapJitterMaxTokens} min={0} suffix="tokens" onChange={(promptCacheCapJitterMaxTokens) => setDraft((prev) => ({ ...prev, promptCacheCapJitterMaxTokens }))} />
          </ConfigGroup>
        )}

        {activeTab === 'usage' && (
          <ConfigGroup icon={<BadgeInfo className="h-4 w-4" />} title="路径级 Usage 上报改写" description="每个路径前缀都是独立覆盖项：先使用未匹配路径默认策略，再按最长匹配路径前缀覆盖。只改变下游响应和后台 usage 记录，不影响本地 reader 计算、缓存 tracker 或上游请求。">
            <div className="space-y-3 md:col-span-2">
              <ReportedUsagePathEditor
                title="未匹配路径默认上报改写"
                description="没有命中 /cc、/ha、/na 等路径覆盖时使用。默认适合 /v1：input/output 使用原始值，cache read/write 保留 high-cache 计算值。"
                value={draft.reportedUsage.default}
                onChange={(defaultPolicy) => setDraft((prev) => ({ ...prev, reportedUsage: { ...prev.reportedUsage, default: defaultPolicy } }))}
              />
              {Object.entries(draft.reportedUsage.pathOverrides).map(([prefix, policy]) => (
                <div key={prefix} className="space-y-3">
                  <FieldLabel title="路径前缀" description="当前前缀只控制它自己匹配到的路径。例如 /cc、/ha、/na 互相独立，后续可以分别改 input、output、cache read、cache write。">
                    <Input
                      bordered
                      size="sm"
                      value={prefix}
                      onChange={(event) => {
                        const nextPrefix = event.target.value
                        setDraft((prev) => {
                          const pathOverrides = { ...prev.reportedUsage.pathOverrides }
                          delete pathOverrides[prefix]
                          pathOverrides[nextPrefix] = policy
                          return { ...prev, reportedUsage: { ...prev.reportedUsage, pathOverrides } }
                        })
                      }}
                    />
                  </FieldLabel>
                  <ReportedUsagePathEditor
                    title={`${prefix || '/'} 覆盖策略`}
                    description="只覆盖这个路径前缀匹配到的请求。关闭后不会把本地模拟 cache usage 展示给下游或后台记录；如果请求本身带有真实上游 metadata usage，仍按真实值处理。"
                    value={policy}
                    onDelete={() =>
                      setDraft((prev) => {
                        const pathOverrides = { ...prev.reportedUsage.pathOverrides }
                        delete pathOverrides[prefix]
                        return { ...prev, reportedUsage: { ...prev.reportedUsage, pathOverrides } }
                      })
                    }
                    onChange={(nextPolicy) =>
                      setDraft((prev) => ({
                        ...prev,
                        reportedUsage: {
                          ...prev.reportedUsage,
                          pathOverrides: { ...prev.reportedUsage.pathOverrides, [prefix]: nextPolicy },
                        },
                      }))
                    }
                  />
                </div>
              ))}
              <div className="flex justify-end">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    setDraft((prev) => {
                      let index = 1
                      let prefix = '/new'
                      while (prev.reportedUsage.pathOverrides[prefix]) {
                        index += 1
                        prefix = `/new-${index}`
                      }
                      return {
                        ...prev,
                        reportedUsage: {
                          ...prev.reportedUsage,
                          pathOverrides: { ...prev.reportedUsage.pathOverrides, [prefix]: pathPolicy() },
                        },
                      }
                    })
                  }
                >
                  添加路径覆盖
                </Button>
              </div>
            </div>
          </ConfigGroup>
        )}

        {activeTab === 'compat' && (
          <>
            <ConfigGroup icon={<Shield className="h-4 w-4" />} title="兼容与诊断" description="控制协议兼容细节和调试信息展示。调试信息只影响响应头或非流式 thinking 解析，不改变凭据调度。">
              <FieldLabel title="兼容模式" description="Claude Code 兼容适合日常 CLI 使用；Anthropic 严格模式会减少代理侧改写；调试模式会默认暴露代理改写告警头。">
                <Select bordered size="sm" value={draft.compatProfile} onChange={(event) => setDraft((prev) => ({ ...prev, compatProfile: event.target.value as CompatProfile }))}>
                  <Select.Option value="claude-code">Claude Code 兼容</Select.Option>
                  <Select.Option value="anthropic-strict">Anthropic 严格模式</Select.Option>
                  <Select.Option value="debug">调试模式</Select.Option>
                </Select>
              </FieldLabel>
              <ToggleField title="提取 Thinking 内容块" description="非流式响应里是否把 <thinking> 标签解析成独立 thinking 内容块。" checked={draft.extractThinking} onChange={(extractThinking) => setDraft((prev) => ({ ...prev, extractThinking }))} />
              <ToggleField title="暴露代理改写告警" description="是否通过 x-kiro-rs-warnings 响应头展示代理侧动作，方便排查兼容问题。" checked={draft.exposeProxyWarnings} onChange={(exposeProxyWarnings) => setDraft((prev) => ({ ...prev, exposeProxyWarnings }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="后台统计" description="控制后台 usage 汇总的判断口径，只影响页面统计，不影响真实请求、缓存计算和费用估算。">
              <NumberField title="高缓存判定阈值" description="后台把一次请求统计为高缓存请求的 cache_read_input_tokens 门槛。" value={draft.highCacheThreshold} min={0} suffix="tokens" onChange={(highCacheThreshold) => setDraft((prev) => ({ ...prev, highCacheThreshold }))} />
            </ConfigGroup>
          </>
        )}

        <Alert status="info" className="py-2 text-sm">
          <Shield className="h-4 w-4" />
          <span>保存前会校验冷却、预热、缓存比例、放大倍数和触顶扣减范围；保存后新请求热加载生效。</span>
        </Alert>
      </div>
    </SectionCard>
  )
}
