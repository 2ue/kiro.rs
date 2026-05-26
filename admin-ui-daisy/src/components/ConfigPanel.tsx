import { BadgeInfo, Gauge, Save, Shield, Sparkles, Trash2, Wand2, Zap } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Alert, Button, Card, Collapse, Input, Join, Loading, Select, Toggle } from 'react-daisyui'
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
          type="number"
          className="join-item w-full"
          value={value}
          min={min}
          max={max}
          step={step}
          inputMode={step ? 'decimal' : 'numeric'}
          onChange={(event) => onChange(numberValue(event.target.value, min ?? 0))}
        />
        <span className="join-item unit-addon min-w-24">{suffix}</span>
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
      <Card.Body className="flex-row items-center justify-between gap-4 p-4">
      <div className="min-w-0">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-1 text-xs leading-5 text-base-content/60">{description}</div>
      </div>
      <Toggle color="primary" className="shrink-0" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
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
      <Collapse.Title className="flex items-start gap-3 px-4 py-3">
        <span className="rounded-lg border border-base-300 bg-base-200 p-2 text-primary">{icon}</span>
        <span>
          <span className="block text-sm font-semibold">{title}</span>
          <span className="mt-1 block text-xs leading-5 text-base-content/60">{description}</span>
        </span>
      </Collapse.Title>
      <Collapse.Content>
        <div className="grid gap-4 border-t border-base-300/70 pt-4 md:grid-cols-2">{children}</div>
      </Collapse.Content>
    </Collapse>
  )
}

function ModeSelect({ value, disabled, onChange }: { value: ReportedUsageFieldMode; disabled?: boolean; onChange: (value: ReportedUsageFieldMode) => void }) {
  return (
    <Select bordered className="w-full" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value as ReportedUsageFieldMode)}>
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
          className="join-item w-full"
          type="number"
          value={value}
          min={min}
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
      <Card.Body className="p-4">
      <div className="mb-3">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-1 text-xs leading-5 text-base-content/60">{description}</div>
      </div>
      <div className="space-y-3">
        <ModeSelect value={value.mode} disabled={disabled} onChange={(mode) => onChange({ ...value, mode })} />
        <div className="rounded-box bg-base-200 px-3 py-2 text-xs leading-5 text-base-content/65">{reportedUsageModeDescription(value.mode)}</div>
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
      <Card.Body className="p-4">
      <div className="mb-4 flex items-start justify-between gap-4">
        <div>
          <h4 className="text-sm font-semibold">{title}</h4>
          <p className="mt-1 text-xs leading-5 text-base-content/60">{description}</p>
        </div>
        <div className="flex items-center gap-2">
          {onDelete && (
            <Button type="button" color="error" variant="outline" size="sm" onClick={onDelete} title="删除这条路径覆盖">
              <Trash2 className="h-4 w-4" />
              删除覆盖
            </Button>
          )}
          <Toggle color="primary" className="shrink-0" checked={value.enabled} onChange={(event) => onChange({ ...value, enabled: event.target.checked })} />
        </div>
      </div>
      {!value.enabled && (
        <Alert status="warning" className="mb-4 text-xs leading-5">
          当前路径已关闭本地模拟缓存上报：下游响应和后台 usage 记录会隐藏模拟 cache read/write，并把 input 展示为完整输入。字段改写配置已隐藏，重新开启后才会显示并生效。
        </Alert>
      )}
      {value.enabled && (
        <div className="grid gap-4 xl:grid-cols-2">
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

  useEffect(() => {
    if (config.data) setDraft(config.data)
  }, [config.data])

  const save = () => {
    const next: RuntimeConfig = {
      ...draft,
      credentialRpm: toWhole(draft.credentialRpm),
      credentialMaxConcurrentRequests: toWhole(draft.credentialMaxConcurrentRequests),
      credentialTransientCooldownSecs: toWhole(draft.credentialTransientCooldownSecs, 1),
      credentialMaxCooldownSecs: toWhole(draft.credentialMaxCooldownSecs, 1),
      credentialDispatchMaxWaitSecs: toWhole(draft.credentialDispatchMaxWaitSecs),
      credentialInFlightLeaseMaxSecs: toWhole(draft.credentialInFlightLeaseMaxSecs),
      credentialWarmupRequests: toWhole(draft.credentialWarmupRequests),
      credentialWarmupSelectionPercent: toWhole(draft.credentialWarmupSelectionPercent, 0, 100),
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
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens) return toast.error('触顶扣减下限不能大于上限')
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
      actions={
        <Button type="button" color="primary" size="sm" onClick={save} disabled={updateConfig.isPending}>
          {updateConfig.isPending ? <Loading size="xs" /> : <Save className="h-4 w-4" />}
          保存
        </Button>
      }
    >
      <div className="space-y-6">
        <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="凭据限速与冷却" description="控制单个账号被调用的频率，以及上游临时错误后多久再尝试使用该账号。">
          <NumberField title="单凭据每分钟请求上限" description="控制每个凭据每分钟最多承接多少请求。填 0 表示关闭本地限速。" value={draft.credentialRpm} min={0} suffix="次/分钟" onChange={(credentialRpm) => setDraft((prev) => ({ ...prev, credentialRpm }))} />
          <NumberField title="单凭据最大并发请求数" description="控制同一个凭据同时处理多少个请求。填 0 表示不限制。" value={draft.credentialMaxConcurrentRequests} min={0} suffix="并发" onChange={(credentialMaxConcurrentRequests) => setDraft((prev) => ({ ...prev, credentialMaxConcurrentRequests }))} />
          <NumberField title="临时冷却秒数" description="当上游返回 429 或临时错误但没有 Retry-After 时，该凭据暂停使用多久。" value={draft.credentialTransientCooldownSecs} min={1} suffix="秒" onChange={(credentialTransientCooldownSecs) => setDraft((prev) => ({ ...prev, credentialTransientCooldownSecs }))} />
          <NumberField title="最大冷却秒数" description="控制单个凭据最长冷却时间。" value={draft.credentialMaxCooldownSecs} min={1} suffix="秒" onChange={(credentialMaxCooldownSecs) => setDraft((prev) => ({ ...prev, credentialMaxCooldownSecs }))} />
          <NumberField title="单请求最长排队等待" description="所有可用凭据都处于冷却、限速或并发占满时最多等待多久。填 0 表示不限制。" value={draft.credentialDispatchMaxWaitSecs} min={0} suffix="秒" onChange={(credentialDispatchMaxWaitSecs) => setDraft((prev) => ({ ...prev, credentialDispatchMaxWaitSecs }))} />
          <NumberField title="异常并发自动回收" description="单个并发占用超过多久未活跃时自动释放。填 0 表示关闭。" value={draft.credentialInFlightLeaseMaxSecs} min={0} suffix="秒" onChange={(credentialInFlightLeaseMaxSecs) => setDraft((prev) => ({ ...prev, credentialInFlightLeaseMaxSecs }))} />
        </ConfigGroup>

        <ConfigGroup icon={<Sparkles className="h-4 w-4" />} title="新凭据预热" description="预热不会伪造成功次数，只会让新账号在均衡模式下更少被选中。">
          <NumberField title="预热剩余请求数" description="新添加凭据默认进入预热状态的请求次数。填 0 表示不预热。" value={draft.credentialWarmupRequests} min={0} suffix="次" onChange={(credentialWarmupRequests) => setDraft((prev) => ({ ...prev, credentialWarmupRequests }))} />
          <NumberField title="预热凭据参与概率" description="balanced 模式下预热凭据参与真实请求调度的概率。值越低，新凭据被调用越少。" value={draft.credentialWarmupSelectionPercent} min={0} max={100} suffix="%" onChange={(credentialWarmupSelectionPercent) => setDraft((prev) => ({ ...prev, credentialWarmupSelectionPercent }))} />
        </ConfigGroup>

        <ConfigGroup icon={<Wand2 className="h-4 w-4" />} title="请求压缩" description="控制发往上游前是否压缩请求内容。默认关闭总开关；如需开启，建议只使用空白压缩。">
          <ToggleField title="启用请求压缩" description="控制是否对上游请求做压缩处理。关闭时不会改变请求内容。" checked={draft.compressionEnabled} onChange={(compressionEnabled) => setDraft((prev) => ({ ...prev, compressionEnabled }))} />
          <ToggleField title="仅压缩空白字符" description="控制压缩时是否只处理多余空白。这是当前推荐的低风险压缩方式。" checked={draft.whitespaceCompression} disabled={!draft.compressionEnabled} onChange={(whitespaceCompression) => setDraft((prev) => ({ ...prev, whitespaceCompression }))} />
        </ConfigGroup>

        <ConfigGroup icon={<Zap className="h-4 w-4" />} title="高缓存模拟" description="控制 /v1/messages 和 /cc/v1/messages 的本地高缓存 usage 模拟。只影响下游看到的统计和后台记录，不影响 count_tokens 计算接口。">
          <NumberField title="缓存读取目标比例" description="cache_read_input_tokens 大致占输入的目标比例。常用值 0.95 到 0.99。" value={draft.promptCacheTargetReadRatio} min={0} max={0.99} step={0.01} suffix="比例" onChange={(promptCacheTargetReadRatio) => setDraft((prev) => ({ ...prev, promptCacheTargetReadRatio }))} />
          <NumberField title="高缓存输入放大倍数" description="控制高缓存模拟时 total input 的放大程度。只影响缓存计算，不代表 input 上报一定放大。" value={draft.promptCacheTokenScale} min={1} max={3} step={0.1} suffix="倍" onChange={(promptCacheTokenScale) => setDraft((prev) => ({ ...prev, promptCacheTokenScale }))} />
          <NumberField title="模拟输入上限" description="高缓存模拟后 total input 的最高值。填 0 表示不设置上限。" value={draft.promptCacheMaxSimulatedInputTokens} min={0} suffix="tokens" onChange={(promptCacheMaxSimulatedInputTokens) => setDraft((prev) => ({ ...prev, promptCacheMaxSimulatedInputTokens }))} />
          <NumberField title="放大启用门槛" description="基础输入达到多少 tokens 后才启用输入放大。" value={draft.promptCacheScaleMinInputTokens} min={0} suffix="tokens" onChange={(promptCacheScaleMinInputTokens) => setDraft((prev) => ({ ...prev, promptCacheScaleMinInputTokens }))} />
          <NumberField title="触顶扣减下限" description="模拟输入达到上限时，最少从上限扣掉多少 tokens。" value={draft.promptCacheCapJitterMinTokens} min={0} suffix="tokens" onChange={(promptCacheCapJitterMinTokens) => setDraft((prev) => ({ ...prev, promptCacheCapJitterMinTokens }))} />
          <NumberField title="触顶扣减上限" description="模拟输入达到上限时，最多从上限扣掉多少 tokens。" value={draft.promptCacheCapJitterMaxTokens} min={0} suffix="tokens" onChange={(promptCacheCapJitterMaxTokens) => setDraft((prev) => ({ ...prev, promptCacheCapJitterMaxTokens }))} />
        </ConfigGroup>

        <ConfigGroup icon={<BadgeInfo className="h-4 w-4" />} title="路径级 Usage 上报改写" description="每个路径前缀都是独立覆盖项：先使用未匹配路径默认策略，再按最长匹配路径前缀覆盖。只改变下游响应和后台 usage 记录，不影响本地 reader 计算、缓存 tracker 或上游请求。">
          <div className="space-y-4 md:col-span-2">
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

        <ConfigGroup icon={<Shield className="h-4 w-4" />} title="兼容与诊断" description="控制协议兼容细节和调试信息展示。调试信息只影响响应头或非流式 thinking 解析，不改变凭据调度。">
          <FieldLabel title="兼容模式" description="Claude Code 兼容适合日常 CLI 使用；Anthropic 严格模式会减少代理侧改写；调试模式会默认暴露代理改写告警头。">
            <Select bordered value={draft.compatProfile} onChange={(event) => setDraft((prev) => ({ ...prev, compatProfile: event.target.value as CompatProfile }))}>
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

        <Alert status="info" className="text-sm">
          <Shield className="h-4 w-4" />
          <span>保存前会校验冷却、预热、缓存比例、放大倍数和触顶扣减范围；保存后新请求热加载生效。</span>
        </Alert>
      </div>
    </SectionCard>
  )
}
