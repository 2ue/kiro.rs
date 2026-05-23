import { useEffect, useState } from 'react'
import { BadgeInfo, Gauge, Save, Shield, Sparkles, Wand2, Zap } from 'lucide-react'
import type { ReactNode } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { CompatProfile, RuntimeConfig } from '@/types/api'

const emptyConfig: RuntimeConfig = {
  credentialRpm: 0,
  credentialTransientCooldownSecs: 10,
  credentialMaxCooldownSecs: 300,
  credentialWarmupRequests: 3,
  credentialWarmupSelectionPercent: 5,
  compressionEnabled: false,
  whitespaceCompression: true,
  promptCacheTargetReadRatio: 0.98,
  promptCacheTokenScale: 1.6,
  promptCacheMaxSimulatedInputTokens: 300000,
  promptCacheCapJitterMinTokens: 12000,
  promptCacheCapJitterMaxTokens: 24000,
  promptCacheScaleMinInputTokens: 20000,
  ccHighCacheReportedCacheCreationTargetTokens: 3000,
  ccHighCacheReportedInputMaxTokens: 96,
  highCacheThreshold: 10000,
  compatProfile: 'claude-code',
  extractThinking: true,
  exposeProxyWarnings: false,
}

function toNumber(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

interface NumberFieldProps {
  title: string
  description: string
  value: number
  min?: number
  max?: number
  step?: number
  suffix?: string
  onChange: (value: number) => void
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
}: NumberFieldProps) {
  return (
    <label className="block rounded-md border bg-background p-4">
      <div className="mb-3 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-sm font-medium">{title}</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">{description}</div>
        </div>
        {suffix && (
          <span className="shrink-0 rounded-md bg-muted px-2 py-1 text-xs text-muted-foreground">
            {suffix}
          </span>
        )}
      </div>
      <Input
        type="number"
        value={value}
        min={min}
        max={max}
        step={step}
        inputMode="numeric"
        onChange={(event) => onChange(toNumber(event.target.value, min ?? 0))}
      />
    </label>
  )
}

interface ToggleFieldProps {
  title: string
  description: string
  checked: boolean
  disabled?: boolean
  onCheckedChange: (checked: boolean) => void
}

function ToggleField({ title, description, checked, disabled, onCheckedChange }: ToggleFieldProps) {
  return (
    <label className="flex items-center justify-between gap-4 rounded-md border bg-background p-4">
      <div className="min-w-0">
        <div className="text-sm font-medium">{title}</div>
        <div className="mt-1 text-xs leading-5 text-muted-foreground">{description}</div>
      </div>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onCheckedChange} />
    </label>
  )
}

interface ConfigSectionProps {
  icon: ReactNode
  title: string
  description: string
  children: ReactNode
}

function ConfigSection({ icon, title, description, children }: ConfigSectionProps) {
  return (
    <section className="rounded-lg border bg-muted/20 p-4">
      <div className="mb-4 flex items-start gap-3">
        <div className="rounded-md border bg-background p-2 text-muted-foreground">{icon}</div>
        <div>
          <h3 className="text-base font-semibold">{title}</h3>
          <p className="mt-1 text-sm leading-6 text-muted-foreground">{description}</p>
        </div>
      </div>
      <div className="grid gap-4 md:grid-cols-2">{children}</div>
    </section>
  )
}

interface SelectFieldProps {
  title: string
  description: string
  value: CompatProfile
  onChange: (value: CompatProfile) => void
}

function SelectField({ title, description, value, onChange }: SelectFieldProps) {
  return (
    <label className="block rounded-md border bg-background p-4">
      <div className="mb-3">
        <div className="text-sm font-medium">{title}</div>
        <div className="mt-1 text-xs leading-5 text-muted-foreground">{description}</div>
      </div>
      <select
        className="h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none ring-offset-background focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
        value={value}
        onChange={(event) => onChange(event.target.value as CompatProfile)}
      >
        <option value="claude-code">Claude Code 兼容</option>
        <option value="anthropic-strict">Anthropic 严格模式</option>
        <option value="debug">调试模式</option>
      </select>
    </label>
  )
}

function toWhole(value: number, min = 0, max?: number): number {
  const normalized = Math.max(min, Math.floor(value || 0))
  return typeof max === 'number' ? Math.min(max, normalized) : normalized
}

function toRatio(value: number): number {
  if (!Number.isFinite(value)) {
    return 0
  }
  return Math.min(0.99, Math.max(0, Number(value.toFixed(4))))
}

function toScale(value: number): number {
  if (!Number.isFinite(value)) {
    return 1
  }
  return Math.min(3, Math.max(1, Number(value.toFixed(2))))
}

export function RuntimeConfigPanel() {
  const config = useRuntimeConfig()
  const updateConfig = useUpdateRuntimeConfig()
  const [draft, setDraft] = useState<RuntimeConfig>(emptyConfig)

  useEffect(() => {
    if (config.data) {
      setDraft(config.data)
    }
  }, [config.data])

  const handleSave = () => {
    const next: RuntimeConfig = {
      ...draft,
      credentialRpm: toWhole(draft.credentialRpm),
      credentialTransientCooldownSecs: toWhole(draft.credentialTransientCooldownSecs, 1),
      credentialMaxCooldownSecs: toWhole(draft.credentialMaxCooldownSecs, 1),
      credentialWarmupRequests: toWhole(draft.credentialWarmupRequests),
      credentialWarmupSelectionPercent: toWhole(draft.credentialWarmupSelectionPercent, 0, 100),
      promptCacheTargetReadRatio: toRatio(draft.promptCacheTargetReadRatio),
      promptCacheTokenScale: toScale(draft.promptCacheTokenScale),
      promptCacheMaxSimulatedInputTokens: toWhole(draft.promptCacheMaxSimulatedInputTokens),
      promptCacheCapJitterMinTokens: toWhole(draft.promptCacheCapJitterMinTokens),
      promptCacheCapJitterMaxTokens: toWhole(draft.promptCacheCapJitterMaxTokens),
      promptCacheScaleMinInputTokens: toWhole(draft.promptCacheScaleMinInputTokens),
      ccHighCacheReportedCacheCreationTargetTokens: toWhole(
        draft.ccHighCacheReportedCacheCreationTargetTokens
      ),
      ccHighCacheReportedInputMaxTokens: toWhole(draft.ccHighCacheReportedInputMaxTokens),
      highCacheThreshold: toWhole(draft.highCacheThreshold),
    }
    if (next.credentialTransientCooldownSecs > next.credentialMaxCooldownSecs) {
      toast.error('临时冷却秒数不能大于最大冷却秒数')
      return
    }
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens) {
      toast.error('触顶扣减下限不能大于上限')
      return
    }
    if (next.ccHighCacheReportedCacheCreationTargetTokens > 0 && next.ccHighCacheReportedInputMaxTokens === 0) {
      toast.error('/cc/v1 未缓存输入上限必须大于 0')
      return
    }
    updateConfig.mutate(next, {
      onSuccess: () => toast.success('配置已更新'),
      onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
    })
  }

  if (config.isLoading) {
    return <div className="py-8 text-center text-muted-foreground">加载中...</div>
  }

  if (config.error) {
    return (
      <Card>
        <CardContent className="py-8 text-center text-destructive">
          {extractErrorMessage(config.error)}
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle className="text-base">运行时配置</CardTitle>
          <CardDescription>
            这些配置会写回配置文件，并对后续新请求热加载生效；监听地址、密钥、代理客户端等启动期配置仍需要改配置文件后重启。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <ConfigSection
            icon={<Gauge className="h-4 w-4" />}
            title="凭据限速与冷却"
            description="控制单个账号被调用的频率，以及上游临时错误后多久再尝试使用该账号。"
          >
            <NumberField
              title="单凭据每分钟请求上限"
              description="控制每个凭据每分钟最多承接多少请求。填 0 表示关闭本地限速；开启后会优先把请求分流给其他可用凭据。"
              value={draft.credentialRpm}
              min={0}
              suffix="次/分钟"
              onChange={(credentialRpm) =>
                setDraft((prev) => ({ ...prev, credentialRpm }))
              }
            />
            <NumberField
              title="临时冷却秒数"
              description="当上游返回 429 或临时错误但没有 Retry-After 时，控制该凭据暂停使用多久。"
              value={draft.credentialTransientCooldownSecs}
              min={1}
              suffix="秒"
              onChange={(credentialTransientCooldownSecs) =>
                setDraft((prev) => ({ ...prev, credentialTransientCooldownSecs }))
              }
            />
            <NumberField
              title="最大冷却秒数"
              description="控制单个凭据最长冷却时间，用来限制 Retry-After 或连续临时错误带来的影响。"
              value={draft.credentialMaxCooldownSecs}
              min={1}
              suffix="秒"
              onChange={(credentialMaxCooldownSecs) =>
                setDraft((prev) => ({ ...prev, credentialMaxCooldownSecs }))
              }
            />
          </ConfigSection>

          <ConfigSection
            icon={<Sparkles className="h-4 w-4" />}
            title="新凭据预热"
            description="预热不会伪造成功次数，只会让新账号在均衡模式下更少被选中，降低刚加入时的调用密度。"
          >
            <NumberField
              title="预热剩余请求数"
              description="控制新添加凭据默认进入预热状态的请求次数。填 0 表示新凭据不进入预热。"
              value={draft.credentialWarmupRequests}
              min={0}
              suffix="次"
              onChange={(credentialWarmupRequests) =>
                setDraft((prev) => ({ ...prev, credentialWarmupRequests }))
              }
            />
            <NumberField
              title="预热凭据参与概率"
              description="控制 balanced 模式下预热凭据参与真实请求调度的概率。值越低，新凭据被调用越少。"
              value={draft.credentialWarmupSelectionPercent}
              min={0}
              max={100}
              suffix="%"
              onChange={(credentialWarmupSelectionPercent) =>
                setDraft((prev) => ({ ...prev, credentialWarmupSelectionPercent }))
              }
            />
          </ConfigSection>

          <ConfigSection
            icon={<Wand2 className="h-4 w-4" />}
            title="请求压缩"
            description="控制发往上游前是否压缩请求内容。默认关闭总开关；如需开启，建议只使用空白压缩。"
          >
            <ToggleField
              title="启用请求压缩"
              description="控制是否对上游请求做压缩处理。关闭时不会改变请求内容。"
              checked={draft.compressionEnabled}
              onCheckedChange={(compressionEnabled) =>
                setDraft((prev) => ({ ...prev, compressionEnabled }))
              }
            />
            <ToggleField
              title="仅压缩空白字符"
              description="控制压缩时是否只处理多余空白。这是当前推荐的低风险压缩方式。"
              checked={draft.whitespaceCompression}
              disabled={!draft.compressionEnabled}
              onCheckedChange={(whitespaceCompression) =>
                setDraft((prev) => ({ ...prev, whitespaceCompression }))
              }
            />
          </ConfigSection>

          <ConfigSection
            icon={<Zap className="h-4 w-4" />}
            title="高缓存模拟"
            description="控制 /v1/messages 和 /cc/v1/messages 的本地高缓存 usage 模拟。只影响下游看到的统计和后台记录，不影响 count_tokens 计算接口。"
          >
            <NumberField
              title="缓存读取目标比例"
              description="控制命中本地缓存后，cache_read_input_tokens 大致占输入的目标比例。常用值 0.95 到 0.99；实际会按会话和请求内容自然浮动。"
              value={draft.promptCacheTargetReadRatio}
              min={0}
              max={0.99}
              step={0.01}
              suffix="比例"
              onChange={(promptCacheTargetReadRatio) =>
                setDraft((prev) => ({ ...prev, promptCacheTargetReadRatio }))
              }
            />
            <NumberField
              title="高缓存输入放大倍数"
              description="控制高缓存模拟时 total input 的放大程度。只对达到启用门槛的较长请求生效，范围 1 到 3。"
              value={draft.promptCacheTokenScale}
              min={1}
              max={3}
              step={0.1}
              suffix="倍"
              onChange={(promptCacheTokenScale) =>
                setDraft((prev) => ({ ...prev, promptCacheTokenScale }))
              }
            />
            <NumberField
              title="模拟输入上限"
              description="控制高缓存模拟后 total input 的最高值。填 0 表示不设置上限；触顶时会结合扣减范围做自然抖动。"
              value={draft.promptCacheMaxSimulatedInputTokens}
              min={0}
              suffix="tokens"
              onChange={(promptCacheMaxSimulatedInputTokens) =>
                setDraft((prev) => ({ ...prev, promptCacheMaxSimulatedInputTokens }))
              }
            />
            <NumberField
              title="放大启用门槛"
              description="控制基础输入达到多少 tokens 后才启用输入放大，避免短测试请求被模拟成异常大请求。"
              value={draft.promptCacheScaleMinInputTokens}
              min={0}
              suffix="tokens"
              onChange={(promptCacheScaleMinInputTokens) =>
                setDraft((prev) => ({ ...prev, promptCacheScaleMinInputTokens }))
              }
            />
            <NumberField
              title="触顶扣减下限"
              description="控制模拟输入达到上限时，最少从上限扣掉多少 tokens，用来避免每次固定卡在同一个上限值。"
              value={draft.promptCacheCapJitterMinTokens}
              min={0}
              suffix="tokens"
              onChange={(promptCacheCapJitterMinTokens) =>
                setDraft((prev) => ({ ...prev, promptCacheCapJitterMinTokens }))
              }
            />
            <NumberField
              title="触顶扣减上限"
              description="控制模拟输入达到上限时，最多从上限扣掉多少 tokens。必须大于或等于触顶扣减下限。"
              value={draft.promptCacheCapJitterMaxTokens}
              min={0}
              suffix="tokens"
              onChange={(promptCacheCapJitterMaxTokens) =>
                setDraft((prev) => ({ ...prev, promptCacheCapJitterMaxTokens }))
              }
            />
          </ConfigSection>

          <ConfigSection
            icon={<BadgeInfo className="h-4 w-4" />}
            title="/cc/v1 与 /ha/v1 下游上报"
            description="控制特殊 high-cache 路径对下游返回的缓存 usage 外观。底层本地 reader 计算仍与 /v1 一致；/cc/v1 会同时改写 writer 和未缓存 input，/ha/v1 只改写未缓存 input。"
          >
            <NumberField
              title="写缓存上报目标"
              description="控制 /cc/v1 有缓存时 cache_creation_input_tokens 的常规目标值。实际会在 0 到目标值约 110% 内自然浮动。填 0 表示关闭这项 /cc writer 改写；/ha/v1 不使用这个 writer 改写。"
              value={draft.ccHighCacheReportedCacheCreationTargetTokens}
              min={0}
              suffix="tokens"
              onChange={(ccHighCacheReportedCacheCreationTargetTokens) =>
                setDraft((prev) => ({ ...prev, ccHighCacheReportedCacheCreationTargetTokens }))
              }
            />
            <NumberField
              title="未缓存输入上限"
              description="控制 /cc/v1 和 /ha/v1 有缓存时 input_tokens 的上限，通常保持几十以内；被压低的那部分会归入 cache_read_input_tokens。"
              value={draft.ccHighCacheReportedInputMaxTokens}
              min={0}
              suffix="tokens"
              onChange={(ccHighCacheReportedInputMaxTokens) =>
                setDraft((prev) => ({ ...prev, ccHighCacheReportedInputMaxTokens }))
              }
            />
          </ConfigSection>

          <ConfigSection
            icon={<Shield className="h-4 w-4" />}
            title="兼容与诊断"
            description="控制协议兼容细节和调试信息展示。调试信息只影响响应头或非流式 thinking 解析，不改变凭据调度。"
          >
            <SelectField
              title="兼容模式"
              description="控制请求转换策略。Claude Code 兼容适合日常 CLI 使用；Anthropic 严格模式会减少代理侧改写；调试模式会默认暴露代理改写告警头。"
              value={draft.compatProfile}
              onChange={(compatProfile) => setDraft((prev) => ({ ...prev, compatProfile }))}
            />
            <ToggleField
              title="提取 Thinking 内容块"
              description="控制非流式响应里是否把 <thinking> 标签解析成独立 thinking 内容块。严格模式下不会暴露未签名 thinking。"
              checked={draft.extractThinking}
              onCheckedChange={(extractThinking) =>
                setDraft((prev) => ({ ...prev, extractThinking }))
              }
            />
            <ToggleField
              title="暴露代理改写告警"
              description="控制是否通过 x-kiro-rs-warnings 响应头展示代理侧的消息合并、tool 清理、thinking 覆写等动作，方便排查兼容问题。"
              checked={draft.exposeProxyWarnings}
              onCheckedChange={(exposeProxyWarnings) =>
                setDraft((prev) => ({ ...prev, exposeProxyWarnings }))
              }
            />
          </ConfigSection>

          <ConfigSection
            icon={<Gauge className="h-4 w-4" />}
            title="后台统计"
            description="控制后台 usage 汇总的判断口径，只影响页面统计，不影响真实请求、缓存计算和费用估算。"
          >
            <NumberField
              title="高缓存判定阈值"
              description="控制后台把一次请求统计为高缓存请求的 cache_read_input_tokens 门槛。保存后新的汇总查询会立即按新阈值计算。"
              value={draft.highCacheThreshold}
              min={0}
              suffix="tokens"
              onChange={(highCacheThreshold) =>
                setDraft((prev) => ({ ...prev, highCacheThreshold }))
              }
            />
          </ConfigSection>

          <div className="rounded-lg border bg-muted/20 p-4">
            <div className="mb-2 flex items-center gap-2 text-sm font-medium">
              <Shield className="h-4 w-4 text-muted-foreground" />
              保存前校验
            </div>
            <div className="text-sm leading-6 text-muted-foreground">
              临时冷却秒数不能大于最大冷却秒数；预热参与概率会限制在 0 到 100 之间；缓存读取目标比例会限制在 0 到 0.99；触顶扣减下限不能大于上限。
            </div>
          </div>

          <div className="flex justify-end">
            <Button onClick={handleSave} disabled={updateConfig.isPending}>
              <Save className="h-4 w-4" />
              {updateConfig.isPending ? '保存中...' : '保存'}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
