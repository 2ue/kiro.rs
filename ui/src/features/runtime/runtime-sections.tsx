import { useMemo, useState } from 'react'
import { Plus, Trash2 } from 'lucide-react'
import { Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, Textarea, Button } from '@/components/ui'
import { pathPolicy, inputSamplePolicy, preserveFieldPolicy } from '@/lib/runtime-config-defaults'
import type {
  ModelCapabilitiesStatus,
  ModelMappingConfig,
  ModelMappingRule,
  PayloadShapingConfig,
  PromptCacheCreationControlConfig,
  ReportedUsageConfig,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
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

// ─── 用量展示规则(reportedUsage) ─────────────────────────────────────────────

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

export function ReportedUsageSection({
  reported, onChange,
}: {
  reported: ReportedUsageConfig
  onChange: (next: ReportedUsageConfig) => void
}) {
  const [newPrefix, setNewPrefix] = useState('')
  const [editingPrefix, setEditingPrefix] = useState<string | null>(null)
  const [prefixDraft, setPrefixDraft] = useState('')
  const overrides = Object.entries(reported.pathOverrides)

  const addOverride = () => {
    const trimmed = newPrefix.trim()
    if (!trimmed) return
    const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
    onChange({
      ...reported,
      pathOverrides: { ...reported.pathOverrides, [withSlash]: structuredClone(reported.default) },
    })
    setNewPrefix('')
  }
  const removeOverride = (prefix: string) => {
    const next = { ...reported.pathOverrides }
    delete next[prefix]
    onChange({ ...reported, pathOverrides: next })
  }
  const setOverride = (prefix: string, policy: ReportedUsagePathPolicy) =>
    onChange({ ...reported, pathOverrides: { ...reported.pathOverrides, [prefix]: policy } })

  const startRename = (prefix: string) => {
    setEditingPrefix(prefix)
    setPrefixDraft(prefix)
  }
  const commitRename = (oldPrefix: string) => {
    const newPrefix = prefixDraft.trim()
    if (!newPrefix || newPrefix === oldPrefix) {
      setEditingPrefix(null)
      return
    }
    const policy = reported.pathOverrides[oldPrefix]
    if (!policy) return
    const next = { ...reported.pathOverrides }
    delete next[oldPrefix]
    next[newPrefix] = policy
    onChange({ ...reported, pathOverrides: next })
    setEditingPrefix(null)
  }

  return (
    <div className="space-y-5">
      <div className="rounded-lg border border-border bg-card p-3">
        <div className="mb-3 text-sm font-semibold">默认入口规则</div>
        <PathPolicyEditor policy={reported.default} onChange={(p) => onChange({ ...reported, default: p })} />
      </div>

      <div className="space-y-3">
        <div className="flex items-center justify-between gap-2">
          <span className="text-sm font-semibold">入口覆盖规则</span>
          <div className="flex items-center gap-2">
            <Input placeholder="入口前缀,如 /cc" value={newPrefix} className="h-8 w-[12rem]"
              onChange={(e) => setNewPrefix(e.target.value)} />
            <Button size="sm" variant="outline" onClick={addOverride}>添加入口</Button>
          </div>
        </div>
        {overrides.length === 0 && (
          <div className="rounded-lg border border-dashed border-border px-3 py-6 text-center text-xs text-muted-foreground">
            暂无入口覆盖规则,所有入口走默认规则
          </div>
        )}
        {overrides.map(([prefix, policy]) => (
          <div key={prefix} className="rounded-lg border border-border bg-card p-3">
            <div className="mb-3 flex items-center justify-between gap-2">
              {editingPrefix === prefix ? (
                <div className="flex flex-1 items-center gap-2">
                  <Input
                    value={prefixDraft}
                    className="h-7 flex-1 font-mono text-sm"
                    onChange={(e) => setPrefixDraft(e.target.value)}
                    onBlur={() => commitRename(prefix)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') commitRename(prefix)
                      if (e.key === 'Escape') setEditingPrefix(null)
                    }}
                    autoFocus
                  />
                </div>
              ) : (
                <code
                  className="cursor-pointer rounded bg-muted px-1.5 py-0.5 text-sm font-semibold hover:bg-muted/70"
                  onClick={() => startRename(prefix)}
                  title="点击编辑入口前缀"
                >
                  {prefix}
                </code>
              )}
              <Button size="sm" variant="ghost" className="text-destructive hover:bg-destructive/10" onClick={() => removeOverride(prefix)}>删除</Button>
            </div>
            <PathPolicyEditor policy={policy} onChange={(p) => setOverride(prefix, p)} />
          </div>
        ))}
      </div>
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

// ─── 自定义缓存路由前缀(definedCacheRoutes) ───────────────────────────────────

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

/** 取出 /dfcache/ 之后的名称部分，用于输入框展示 */
function getDefinedCacheRouteName(route: string): string {
  const normalized = normalizeDefinedCacheRoute(route)
  if (normalized) return normalized.slice(DFCACHE_ROUTE_PREFIX.length)
  const trimmed = route.trim()
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  if (withSlash.toLowerCase().startsWith(DFCACHE_ROUTE_PREFIX)) {
    return withSlash.slice(DFCACHE_ROUTE_PREFIX.length)
  }
  return trimmed
}

function definedCacheRouteFromNameInput(name: string): string {
  const trimmed = name.trim()
  const normalizedFullRoute = normalizeDefinedCacheRoute(trimmed)
  if (
    normalizedFullRoute &&
    (trimmed.startsWith('/') || trimmed.toLowerCase().startsWith(DFCACHE_ROUTE_PREFIX.slice(1)))
  ) {
    return normalizedFullRoute
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

/**
 * 自定义高缓存路由编辑：固定前缀 /dfcache/，用户只填名称。
 * 增/改/删时同步维护 reportedUsage.pathOverrides（以完整 route 为 key），与后端语义一致。
 */
export function DefinedCacheRoutesSection({
  routes,
  reported,
  onChange,
}: {
  routes: string[]
  reported: ReportedUsageConfig
  onChange: (routes: string[], reported: ReportedUsageConfig) => void
}) {
  const addRoute = () => {
    let index = routes.length + 1
    const existing = new Set(routes)
    let route = `${DFCACHE_ROUTE_PREFIX}route-${index}`
    while (existing.has(route)) {
      index += 1
      route = `${DFCACHE_ROUTE_PREFIX}route-${index}`
    }
    const pathOverrides = {
      ...reported.pathOverrides,
      [route]: reported.pathOverrides[route] ?? pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy()),
    }
    onChange([...routes, route], { ...reported, pathOverrides })
  }

  const editRoute = (index: number, nameInput: string) => {
    const nextRoute = definedCacheRouteFromNameInput(nameInput)
    const nextRoutes = [...routes]
    const prevRoute = nextRoutes[index]
    nextRoutes[index] = nextRoute
    const pathOverrides = { ...reported.pathOverrides }
    const normPrev = normalizeDefinedCacheRoute(prevRoute)
    const normNext = normalizeDefinedCacheRoute(nextRoute)
    if (normPrev && pathOverrides[normPrev] && normNext && !pathOverrides[normNext]) {
      pathOverrides[normNext] = pathOverrides[normPrev]
      delete pathOverrides[normPrev]
    }
    onChange(nextRoutes, { ...reported, pathOverrides })
  }

  const removeRoute = (index: number) => {
    const target = routes[index]
    const nextRoutes = routes.filter((_, i) => i !== index)
    const pathOverrides = { ...reported.pathOverrides }
    const norm = normalizeDefinedCacheRoute(target)
    if (norm) delete pathOverrides[norm]
    onChange(nextRoutes, { ...reported, pathOverrides })
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="text-xs leading-5 text-muted-foreground">
          只需填写 <code className="rounded bg-muted px-1 py-0.5">/dfcache/</code> 后面的名称。未在此定义的 <code className="rounded bg-muted px-1 py-0.5">/dfcache/*</code> 请求会直接报错。
        </div>
        <Button type="button" variant="outline" size="sm" onClick={addRoute}>
          <Plus className="h-4 w-4" />添加路由
        </Button>
      </div>
      {routes.length === 0 ? (
        <div className="rounded-md border border-dashed border-border px-3 py-3 text-sm text-muted-foreground">
          暂未定义自定义路由。
        </div>
      ) : (
        <div className="space-y-2">
          {routes.map((route, index) => {
            const name = getDefinedCacheRouteName(route)
            const invalid = name.trim().length > 0 && !normalizeDefinedCacheRoute(route)
            return (
              <div key={`${route}-${index}`} className="flex items-center gap-2">
                <div
                  className={`flex min-w-0 flex-1 overflow-hidden rounded-md border bg-background focus-within:ring-2 focus-within:ring-ring ${invalid ? 'border-destructive' : 'border-input'}`}
                >
                  <span className="inline-flex shrink-0 select-none items-center border-r border-border bg-muted/50 px-3 font-mono text-sm text-muted-foreground">
                    {DFCACHE_ROUTE_PREFIX}
                  </span>
                  <Input
                    className="min-w-0 flex-1 rounded-none border-0 font-mono focus-visible:ring-0"
                    value={name}
                    placeholder="cc"
                    onChange={(e) => editRoute(index, e.target.value)}
                  />
                </div>
                <Button type="button" variant="outline" size="icon-sm" title="删除路由" onClick={() => removeRoute(index)}>
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            )
          })}
          <div className="text-xs text-muted-foreground">
            名称仅允许小写字母、数字、点、下划线或短横线，长度不超过 64。
          </div>
        </div>
      )}
    </div>
  )
}
