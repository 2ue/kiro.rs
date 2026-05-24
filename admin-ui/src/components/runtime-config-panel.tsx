import { useEffect, useState } from 'react'
import { BadgeInfo, Gauge, Save, Shield, Sparkles, Trash2, Wand2, Zap } from 'lucide-react'
import type { ReactNode } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type {
  CompatProfile,
  ReportedUsageConfig,
  ReportedUsageFieldMode,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RuntimeConfig,
} from '@/types/api'

const preserveFieldPolicy = (): ReportedUsageFieldPolicy => ({
  mode: 'preserve',
  maxTokens: 0,
  targetTokens: 0,
  normalMaxMultiplier: 1.1,
  moveDeltaToCacheRead: false,
})

const inputSamplePolicy = (maxTokens = 96): ReportedUsageFieldPolicy => ({
  ...preserveFieldPolicy(),
  mode: 'sample-max',
  maxTokens,
  moveDeltaToCacheRead: true,
})

const writerSamplePolicy = (
  targetTokens = 3000,
  normalMaxMultiplier = 1.2
): ReportedUsageFieldPolicy => ({
  ...preserveFieldPolicy(),
  mode: 'sample-target',
  targetTokens,
  normalMaxMultiplier,
})

const pathPolicy = (
  enabled = true,
  input: ReportedUsageFieldPolicy = preserveFieldPolicy(),
  cacheCreation: ReportedUsageFieldPolicy = preserveFieldPolicy()
): ReportedUsagePathPolicy => ({
  enabled,
  input,
  output: preserveFieldPolicy(),
  cacheRead: preserveFieldPolicy(),
  cacheCreation,
})

const defaultReportedUsage = (): ReportedUsageConfig => ({
  default: pathPolicy(),
  pathOverrides: {
    '/na': pathPolicy(false),
    '/cc': pathPolicy(true, inputSamplePolicy(96), writerSamplePolicy(3000)),
    '/ha': pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy()),
  },
})

const emptyConfig: RuntimeConfig = {
  credentialRpm: 0,
  credentialMaxConcurrentRequests: 0,
  credentialTransientCooldownSecs: 10,
  credentialMaxCooldownSecs: 300,
  credentialDispatchMaxWaitSecs: 120,
  credentialInFlightLeaseMaxSecs: 900,
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
  reportedUsage: defaultReportedUsage(),
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
      <div className="mb-3">
        <div className="text-sm font-medium">{title}</div>
        <div className="mt-1 text-xs leading-5 text-muted-foreground">{description}</div>
      </div>
      <div className="flex items-center gap-2">
        <Input
          type="number"
          value={value}
          min={min}
          max={max}
          step={step}
          inputMode="numeric"
          onChange={(event) => onChange(toNumber(event.target.value, min ?? 0))}
        />
        {suffix && (
          <span className="min-w-16 shrink-0 rounded-md border bg-muted px-3 py-2 text-center text-sm text-muted-foreground">
            {suffix}
          </span>
        )}
      </div>
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

interface ModeSelectProps {
  value: ReportedUsageFieldMode
  disabled?: boolean
  onChange: (value: ReportedUsageFieldMode) => void
}

function ModeSelect({ value, disabled, onChange }: ModeSelectProps) {
  return (
    <select
      className="h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none ring-offset-background focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
      value={value}
      disabled={disabled}
      onChange={(event) => onChange(event.target.value as ReportedUsageFieldMode)}
    >
      <option value="preserve">保留计算值（不改写）</option>
      <option value="sample-max">按上限采样改写</option>
      <option value="sample-target">按目标采样改写</option>
    </select>
  )
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

interface PolicyNumberInputProps {
  title: string
  description: string
  value: number
  min?: number
  step?: number
  suffix: string
  disabled?: boolean
  onChange: (value: number) => void
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
}: PolicyNumberInputProps) {
  return (
    <label className="grid gap-2 rounded-md border bg-muted/20 p-3">
      <span className="text-xs font-medium">{title}</span>
      <span className="text-xs leading-5 text-muted-foreground">{description}</span>
      <div className="flex items-center gap-2">
        <Input
          type="number"
          value={value}
          min={min}
          step={step}
          inputMode={step ? 'decimal' : 'numeric'}
          disabled={disabled}
          onChange={(event) => onChange(toNumber(event.target.value, min ?? 0))}
        />
        <span className="min-w-16 shrink-0 rounded-md border bg-background px-3 py-2 text-center text-sm text-muted-foreground">
          {suffix}
        </span>
      </div>
    </label>
  )
}

function reportedUsageModeDescription(mode: ReportedUsageFieldMode): string {
  switch (mode) {
    case 'preserve':
      return '保留计算值表示不对这个字段做二次改写，直接使用 high-cache、上游 metadata 或估算完成后的当前字段值。'
    case 'sample-max':
      return '按上限采样改写会把这个字段改写到上限以内，分布偏向较小值，不会固定贴着上限。需要配置“采样上限”。'
    case 'sample-target':
      return '按目标采样改写会围绕目标值生成自然浮动结果，常规最大值由“目标 tokens × 常规最大倍率”决定，并且不会超过当前可用字段值。'
  }
}

interface ReportedUsageFieldEditorProps {
  title: string
  description: string
  value: ReportedUsageFieldPolicy
  allowMoveDelta?: boolean
  disabled?: boolean
  onChange: (value: ReportedUsageFieldPolicy) => void
}

function ReportedUsageFieldEditor({
  title,
  description,
  value,
  allowMoveDelta,
  disabled,
  onChange,
}: ReportedUsageFieldEditorProps) {
  return (
    <div className="rounded-md border bg-background p-4">
      <div className="mb-3">
        <div className="text-sm font-medium">{title}</div>
        <div className="mt-1 text-xs leading-5 text-muted-foreground">{description}</div>
      </div>
      <div className="grid gap-3">
        <ModeSelect
          value={value.mode}
          disabled={disabled}
          onChange={(mode) => onChange({ ...value, mode })}
        />
        <div className="rounded-md bg-muted/40 px-3 py-2 text-xs leading-5 text-muted-foreground">
          {reportedUsageModeDescription(value.mode)}
        </div>
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
          <div className="grid gap-3 sm:grid-cols-2">
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
              description="控制正常随机范围的上限，常规最大值 = 目标值 × 倍率。比如 3000 和 1.2 表示正常最高约 3600。"
              value={value.normalMaxMultiplier}
              min={1}
              step={0.1}
              suffix="倍"
              disabled={disabled}
              onChange={(normalMaxMultiplier) =>
                onChange({ ...value, normalMaxMultiplier })
              }
            />
          </div>
        )}
        {allowMoveDelta && (
          <ToggleField
            title="差值计入缓存读取"
            description="开启后，input_tokens 被压低的差值会加到 cache_read_input_tokens，只改变下游上报外观。"
            checked={value.moveDeltaToCacheRead}
            disabled={disabled || value.mode === 'preserve'}
            onCheckedChange={(moveDeltaToCacheRead) =>
              onChange({ ...value, moveDeltaToCacheRead })
            }
          />
        )}
      </div>
    </div>
  )
}

interface ReportedUsagePathEditorProps {
  title: string
  description: string
  value: ReportedUsagePathPolicy
  onDelete?: () => void
  onChange: (value: ReportedUsagePathPolicy) => void
}

function ReportedUsagePathEditor({
  title,
  description,
  value,
  onDelete,
  onChange,
}: ReportedUsagePathEditorProps) {
  return (
    <div className="rounded-lg border bg-muted/20 p-4">
      <div className="mb-4 flex items-start justify-between gap-4">
        <div>
          <h4 className="text-sm font-semibold">{title}</h4>
          <p className="mt-1 text-xs leading-5 text-muted-foreground">{description}</p>
        </div>
        <div className="flex items-center gap-2">
          {onDelete && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="text-muted-foreground hover:text-destructive"
              onClick={onDelete}
              title="删除这条路径覆盖"
            >
              <Trash2 className="h-4 w-4" />
              删除覆盖
            </Button>
          )}
          <Switch
            checked={value.enabled}
            onCheckedChange={(enabled) => onChange({ ...value, enabled })}
          />
        </div>
      </div>
      {!value.enabled && (
        <div className="mb-4 rounded-md border border-amber-200 bg-amber-50 px-4 py-3 text-xs leading-5 text-amber-900">
          当前路径已关闭本地模拟缓存上报：下游响应和后台 usage 记录会隐藏模拟 cache read/write，
          并把 input 展示为完整输入。字段改写配置已隐藏，重新开启后才会显示并生效。
        </div>
      )}
      {value.enabled && (
        <div className="grid gap-4 lg:grid-cols-2">
          <ReportedUsageFieldEditor
            title="输入字段改写（input_tokens）"
            description="控制计算完成后给下游和后台记录的 input_tokens。保留计算值表示不再二次改写；常见做法是采样到几十以内，并把差值计入缓存读取。"
            value={value.input}
            allowMoveDelta
            onChange={(input) => onChange({ ...value, input })}
          />
          <ReportedUsageFieldEditor
            title="输出字段改写（output_tokens）"
            description="控制计算完成后给下游和后台记录的 output_tokens。默认建议保留计算值，避免影响客户端对输出量的判断。"
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
    </div>
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

function toMultiplier(value: number): number {
  if (!Number.isFinite(value)) {
    return 1.1
  }
  return Math.max(1, Number(value.toFixed(2)))
}

function normalizeFieldPolicy(policy: ReportedUsageFieldPolicy): ReportedUsageFieldPolicy {
  return {
    ...policy,
    maxTokens: toWhole(policy.maxTokens),
    targetTokens: toWhole(policy.targetTokens),
    normalMaxMultiplier: toMultiplier(policy.normalMaxMultiplier),
  }
}

function normalizePathPolicy(policy: ReportedUsagePathPolicy): ReportedUsagePathPolicy {
  return {
    ...policy,
    input: normalizeFieldPolicy(policy.input),
    output: normalizeFieldPolicy(policy.output),
    cacheRead: normalizeFieldPolicy(policy.cacheRead),
    cacheCreation: normalizeFieldPolicy(policy.cacheCreation),
  }
}

function normalizeReportedUsage(config: ReportedUsageConfig): ReportedUsageConfig {
  const pathOverrides = Object.fromEntries(
    Object.entries(config.pathOverrides)
      .map(([prefix, policy]) => {
        const trimmed = prefix.trim()
        if (!trimmed) {
          return null
        }
        const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
        const normalizedPrefix = withSlash.replace(/\/+$/, '') || '/'
        return [normalizedPrefix, normalizePathPolicy(policy)] as const
      })
      .filter((entry): entry is readonly [string, ReportedUsagePathPolicy] => Boolean(entry))
  )
  return {
    default: normalizePathPolicy(config.default),
    pathOverrides,
  }
}

function fieldNeedsMax(policy: ReportedUsageFieldPolicy): boolean {
  return policy.mode === 'sample-max'
}

function fieldNeedsTarget(policy: ReportedUsageFieldPolicy): boolean {
  return policy.mode === 'sample-target'
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
    if (next.credentialTransientCooldownSecs > next.credentialMaxCooldownSecs) {
      toast.error('临时冷却秒数不能大于最大冷却秒数')
      return
    }
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens) {
      toast.error('触顶扣减下限不能大于上限')
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
            这些配置会写入 PgSQL 并对后续新请求热加载生效；监听地址、密钥、数据库连接和代理客户端等启动期配置仍需要改启动配置后重启。
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
              title="单凭据最大并发请求数"
              description="控制同一个凭据同时处理多少个请求。填 0 表示不限制；填 1 表示该凭据一次只跑一个请求，其他请求优先换到别的凭据。"
              value={draft.credentialMaxConcurrentRequests}
              min={0}
              suffix="并发"
              onChange={(credentialMaxConcurrentRequests) =>
                setDraft((prev) => ({ ...prev, credentialMaxConcurrentRequests }))
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
            <NumberField
              title="单请求最长排队等待"
              description="控制请求在所有可用凭据都处于冷却、限速或并发占满时最多等待多久。填 0 表示不限制；建议生产设置为 60 到 180 秒，避免客户端一直挂起。"
              value={draft.credentialDispatchMaxWaitSecs}
              min={0}
              suffix="秒"
              onChange={(credentialDispatchMaxWaitSecs) =>
                setDraft((prev) => ({ ...prev, credentialDispatchMaxWaitSecs }))
              }
            />
            <NumberField
              title="异常并发自动回收"
              description="控制单个并发占用超过多久未活跃时自动释放。填 0 表示关闭；建议大于正常长请求耗时，避免异常路径把账号永久占满。"
              value={draft.credentialInFlightLeaseMaxSecs}
              min={0}
              suffix="秒"
              onChange={(credentialInFlightLeaseMaxSecs) =>
                setDraft((prev) => ({ ...prev, credentialInFlightLeaseMaxSecs }))
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
            title="路径级 Usage 上报改写"
            description="每个路径前缀都是独立覆盖项：先使用未匹配路径的默认改写策略，再按最长匹配的路径前缀覆盖。这里处理的是 high-cache、上游 metadata 或估算完成后的 usage 投影；只改变下游响应和后台 usage 记录，不影响本地 reader 计算、缓存 tracker 或上游请求。"
          >
            <div className="md:col-span-2 space-y-4">
              <ReportedUsagePathEditor
                title="未匹配路径默认上报改写"
                description="没有命中 /cc、/ha、/na 等路径覆盖时使用。默认适合 /v1：保留 high-cache 计算后的 usage，不额外压 input、reader、writer 或 output。"
                value={draft.reportedUsage.default}
                onChange={(defaultPolicy) =>
                  setDraft((prev) => ({
                    ...prev,
                    reportedUsage: { ...prev.reportedUsage, default: defaultPolicy },
                  }))
                }
              />
              {Object.entries(draft.reportedUsage.pathOverrides).map(([prefix, policy]) => (
                <div key={prefix} className="space-y-3">
                  <label className="block rounded-md border bg-background p-4">
                    <div className="mb-3">
                      <div className="text-sm font-medium">路径前缀</div>
                      <div className="mt-1 text-xs leading-5 text-muted-foreground">
                        当前前缀只控制它自己匹配到的路径。例如 /cc、/ha、/na 互相独立，后续可以分别改 input、output、cache read、cache write。
                      </div>
                    </div>
                    <Input
                      value={prefix}
                      onChange={(event) => {
                        const nextPrefix = event.target.value
                        setDraft((prev) => {
                          const pathOverrides = { ...prev.reportedUsage.pathOverrides }
                          delete pathOverrides[prefix]
                          pathOverrides[nextPrefix] = policy
                          return {
                            ...prev,
                            reportedUsage: { ...prev.reportedUsage, pathOverrides },
                          }
                        })
                      }}
                    />
                  </label>
                  <ReportedUsagePathEditor
                    title={`${prefix || '/'} 覆盖策略`}
                    description="只覆盖这个路径前缀匹配到的请求。关闭后不会把本地模拟 cache usage 展示给下游或后台记录；如果请求本身带有真实上游 metadata usage，仍按真实值处理。"
                    value={policy}
                    onDelete={() =>
                      setDraft((prev) => {
                        const pathOverrides = { ...prev.reportedUsage.pathOverrides }
                        delete pathOverrides[prefix]
                        return {
                          ...prev,
                          reportedUsage: { ...prev.reportedUsage, pathOverrides },
                        }
                      })
                    }
                    onChange={(nextPolicy) =>
                      setDraft((prev) => ({
                        ...prev,
                        reportedUsage: {
                          ...prev.reportedUsage,
                          pathOverrides: {
                            ...prev.reportedUsage.pathOverrides,
                            [prefix]: nextPolicy,
                          },
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
                  onClick={() =>
                    setDraft((prev) => {
                      const base = '/new'
                      let index = 1
                      let prefix = base
                      while (prev.reportedUsage.pathOverrides[prefix]) {
                        index += 1
                        prefix = `${base}-${index}`
                      }
                      return {
                        ...prev,
                        reportedUsage: {
                          ...prev.reportedUsage,
                          pathOverrides: {
                            ...prev.reportedUsage.pathOverrides,
                            [prefix]: pathPolicy(),
                          },
                        },
                      }
                    })
                  }
                >
                  添加路径覆盖
                </Button>
              </div>
            </div>
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
