import { Link } from 'react-router-dom'
import { Badge } from '@/components/ui'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import type {
  InjectionCatalogEntry,
  InjectionId,
  InjectionItemOverride,
  RouteFeatureOverride,
  RouteFeatureSet,
  RuntimeConfig,
  ThinkingTriggerMode,
} from '@/types/api'
import {
  THINKING_TRIGGER_LABEL,
  dependencyBlock,
  effectiveFeatures,
  inheritedFeatures,
  inheritedItem,
} from './injection-model'

type Tri = 'inherit' | 'on' | 'off'

function toTri(value: boolean | undefined): Tri {
  return value === undefined ? 'inherit' : value ? 'on' : 'off'
}

function fromTri(value: string): boolean | undefined {
  return value === 'inherit' ? undefined : value === 'on'
}

function TriSelect({
  id,
  value,
  inherited,
  disabled,
  onChange,
}: {
  id: string
  value: boolean | undefined
  inherited: boolean
  disabled?: boolean
  onChange: (value: boolean | undefined) => void
}) {
  return (
    <Select value={toTri(value)} disabled={disabled} onValueChange={(raw) => onChange(fromTri(raw))}>
      <SelectTrigger id={id} size="sm"><SelectValue /></SelectTrigger>
      <SelectContent>
        <SelectItem value="inherit">{`跟随默认（当前${inherited ? '开' : '关'}）`}</SelectItem>
        <SelectItem value="on">开启</SelectItem>
        <SelectItem value="off">关闭</SelectItem>
      </SelectContent>
    </Select>
  )
}


/** What one route-editor row stands for: a prompt-injection item or a route feature. */
type GroupRow =
  | { kind: 'item'; id: InjectionId; note?: string }
  | { kind: 'feature'; key: Exclude<keyof RouteFeatureSet, 'thinkingTrigger'>; note?: string }
  | { kind: 'thinkingTrigger' }

interface Group {
  title: string
  desc: string
  rows: GroupRow[]
}

/**
 * Items and features that act on the same thing share a group, so a route's identity, language or
 * thinking behaviour is decided in one place. Rows inside a group follow their dependency order.
 */
const GROUPS: Group[] = [
  {
    title: '身份与渠道',
    desc: '模型怎么介绍自己，以及回复里暴露上游的句子要不要删掉。',
    rows: [
      { kind: 'item', id: 'identityPrompt' },
      { kind: 'item', id: 'identityTurnReminder', note: '配合身份声明使用' },
      { kind: 'feature', key: 'channelLeakGuard' },
      { kind: 'feature', key: 'channelLeakGuardStream', note: '需要先开渠道信息输出过滤' },
    ],
  },
  {
    title: '回复语言',
    desc: '可见回复用什么语言。',
    rows: [
      { kind: 'item', id: 'languageConstraint' },
      { kind: 'item', id: 'languageTurnReminder', note: '配合语言约束使用' },
    ],
  },
  {
    title: '任务引导',
    desc: '写进 system 的额外规则。',
    rows: [
      { kind: 'item', id: 'taskQuality' },
      { kind: 'item', id: 'custom' },
    ],
  },
  {
    title: 'thinking',
    desc: '什么时候开启 thinking，以及在没有原生 reasoning 字段的模型上怎么开。',
    rows: [
      { kind: 'thinkingTrigger' },
      { kind: 'feature', key: 'thinkingModelName' },
      { kind: 'item', id: 'thinkingControls' },
      { kind: 'item', id: 'thinkingOutputPolicy', note: '需要 thinking 控制标签已注入' },
    ],
  },
  {
    title: '工具调用',
    desc: 'tool_choice 约束和大文件写入。',
    rows: [
      { kind: 'item', id: 'toolChoicePrefix' },
      { kind: 'item', id: 'toolChoiceTurnReminder', note: '配合 tool_choice 前缀使用' },
      { kind: 'item', id: 'chunkedWritePolicy' },
      { kind: 'item', id: 'chunkedWriteToolSuffix' },
    ],
  },
  {
    title: 'WebSearch',
    desc: '请求带原生 web_search 工具时的处理方式。',
    rows: [
      { kind: 'feature', key: 'modelDrivenWebSearch' },
      { kind: 'item', id: 'webSearchGuidance', note: '只在 WebSearch 模型驱动开启时注入' },
    ],
  },
  {
    title: '故障恢复',
    desc: '上游没有输出时的重试。',
    rows: [{ kind: 'item', id: 'emptyTurnNudge' }],
  },
]

const FEATURE_TEXT: Record<Exclude<keyof RouteFeatureSet, 'thinkingTrigger'>, { label: string; desc: string }> = {
  thinkingModelName: {
    label: '-thinking 模型名',
    desc: '请求 claude-xxx-thinking 这类模型名时，自动补上 thinking 参数。关闭后这类模型名按普通模型处理。',
  },
  channelLeakGuard: {
    label: '渠道信息输出过滤',
    desc: '删除回复里"我是 Kiro""指令冲突"这类暴露上游的句子。正则较宽，讨论 kiro.rs 代码时可能误删正常句子。',
  },
  channelLeakGuardStream: {
    label: '流式也过滤',
    desc: '流式回复按整句检查后再下发，首字会稍晚。',
  },
  modelDrivenWebSearch: {
    label: 'WebSearch 模型驱动',
    desc: '由模型决定搜索词并写带引用的回答；每次搜索多调用 1 次模型，token 计入本次 usage。关闭时沿用模板回答。',
  },
}

function Row({ title, code, badge, note, control, children }: {
  title: string
  code?: string
  badge?: React.ReactNode
  note?: string
  control: React.ReactNode
  children?: React.ReactNode
}) {
  return (
    <div className="grid gap-x-4 gap-y-1.5 py-3 first:pt-0 last:pb-0 sm:grid-cols-[minmax(0,1fr)_14rem] sm:items-start">
      <div className="min-w-0 space-y-1">
        <div className="flex flex-wrap items-center gap-1.5">
          {code && <Badge size="xs" tone="neutral">{code}</Badge>}
          <span className="text-sm font-medium">{title}</span>
          {badge}
          {note && <span className="text-[11px] text-muted-foreground">· {note}</span>}
        </div>
        {children}
      </div>
      <div className="min-w-0">{control}</div>
    </div>
  )
}

/**
 * One route's prompt-injection items and route features, grouped by what they act on.
 * `injection` and `features` hold only this route's explicit settings; `config` is the saved
 * runtime config used to show inherited values and dependencies.
 */
export function RouteBehaviorEditor({
  route,
  catalog,
  config,
  injection,
  features,
  onInjectionChange,
  onFeaturesChange,
}: {
  route: string
  catalog: InjectionCatalogEntry[]
  config: RuntimeConfig
  injection: Partial<Record<string, InjectionItemOverride>>
  features: RouteFeatureOverride
  onInjectionChange: (next: Partial<Record<string, InjectionItemOverride>>) => void
  onFeaturesChange: (next: RouteFeatureOverride) => void
}) {
  const working: RuntimeConfig = {
    ...config,
    promptInjection: {
      ...config.promptInjection,
      pathOverrides: { ...config.promptInjection.pathOverrides, [route]: injection },
    },
    routeFeatures: {
      ...config.routeFeatures,
      pathOverrides: { ...config.routeFeatures.pathOverrides, [route]: features },
    },
  }
  const entries = new Map(catalog.map((entry) => [entry.id, entry]))
  const inheritedFeatureSet = inheritedFeatures(config.routeFeatures, route)
  const effectiveFeatureSet = effectiveFeatures(working.routeFeatures, route)

  const setItem = (id: string, patch: InjectionItemOverride) => {
    const next = { ...injection, [id]: { ...(injection[id] ?? {}), ...patch } }
    const item = next[id]!
    if (item.enabled === undefined && item.externalPool === undefined) delete next[id]
    onInjectionChange(next)
  }
  const setFeature = <K extends keyof RouteFeatureSet>(key: K, next: RouteFeatureSet[K] | undefined) => {
    const updated = { ...features }
    if (next === undefined) delete updated[key]
    else updated[key] = next
    onFeaturesChange(updated)
  }

  const renderItem = (id: InjectionId, note?: string) => {
    const entry = entries.get(id)
    if (!entry) return null
    const own = injection[id] ?? {}
    const inherited = inheritedItem(config.promptInjection, route, id)
    const effectiveEnabled = own.enabled ?? inherited.enabled
    const blocked = effectiveEnabled ? dependencyBlock(entry, working, route) : null
    const controlId = `route-injection-${id}`
    const text = entry.texts[0]
      ? (config.promptInjection.texts[entry.texts[0].key] ?? entry.texts[0].defaultText)
      : entry.readOnlyVariants[0]?.defaultText ?? ''
    const warnOff = entry.recommendation === 'keep_on' && !effectiveEnabled
    return (
      <Row
        key={id}
        title={entry.name}
        code={entry.code}
        note={note}
        badge={entry.recommendation === 'keep_on' ? <Badge size="xs" tone="warning">建议开</Badge> : undefined}
        control={
          <div className="space-y-1.5">
            <TriSelect id={controlId} value={own.enabled} inherited={inherited.enabled} onChange={(enabled) => setItem(id, { enabled })} />
            {entry.externalPool && (
              <label className="flex items-center justify-between gap-2 text-[11px] text-muted-foreground" htmlFor={`${controlId}-external`}>
                <span className="shrink-0">外部池</span>
                <span className="min-w-0 flex-1">
                  <TriSelect
                    id={`${controlId}-external`}
                    value={own.externalPool}
                    inherited={inherited.externalPool}
                    disabled={!effectiveEnabled}
                    onChange={(externalPool) => setItem(id, { externalPool })}
                  />
                </span>
              </label>
            )}
          </div>
        }
      >
        <p className="text-xs leading-5 text-muted-foreground">位置：{entry.position}。{entry.trigger}。</p>
        {id === 'custom' && !text.trim() && (
          <p className="text-xs leading-5 text-muted-foreground">还没有填写文本，开启也不会注入。</p>
        )}
        {blocked && <p className="text-xs text-warning">不生效：{blocked}</p>}
        {warnOff && <p className="text-xs text-destructive">关闭后：{entry.impactWhenOff}</p>}
        {text.trim() && (
          <p className="line-clamp-2 break-all font-mono text-[0.68rem] leading-4 text-muted-foreground/80" title={text}>
            {text.slice(0, 200)}
          </p>
        )}
      </Row>
    )
  }

  const renderFeature = (key: Exclude<keyof RouteFeatureSet, 'thinkingTrigger'>, note?: string) => {
    const id = `route-feature-${key}`
    const disabled = key === 'channelLeakGuardStream' && !effectiveFeatureSet.channelLeakGuard
    return (
      <Row
        key={key}
        title={FEATURE_TEXT[key].label}
        badge={<Badge size="xs" tone="secondary">功能</Badge>}
        note={note}
        control={
          <TriSelect
            id={id}
            value={features[key]}
            inherited={inheritedFeatureSet[key]}
            disabled={disabled}
            onChange={(next) => setFeature(key, next)}
          />
        }
      >
        <p className="text-xs leading-5 text-muted-foreground">{FEATURE_TEXT[key].desc}</p>
      </Row>
    )
  }

  const renderThinkingTrigger = () => (
    <Row
      key="thinkingTrigger"
      title="thinking 触发方式"
      badge={<Badge size="xs" tone="secondary">功能</Badge>}
      control={
        <Select
          value={features.thinkingTrigger ?? 'inherit'}
          onValueChange={(raw) => setFeature('thinkingTrigger', raw === 'inherit' ? undefined : (raw as ThinkingTriggerMode))}
        >
          <SelectTrigger id="route-feature-thinking-trigger" size="sm" aria-label="thinking 触发方式"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="inherit">{`跟随默认（当前${THINKING_TRIGGER_LABEL[inheritedFeatureSet.thinkingTrigger]}）`}</SelectItem>
            <SelectItem value="real_request">按真实请求</SelectItem>
            <SelectItem value="always">总是开启</SelectItem>
            <SelectItem value="off">不自动开启</SelectItem>
          </SelectContent>
        </Select>
      }
    >
      <p className="text-xs leading-5 text-muted-foreground">
        按真实请求：用户消息里有 Claude Code 的深度思考信号（如 ultrathink）时开启并强制可见。总是开启：请求没有关闭 thinking 时都开启。不自动开启：只看客户端自己的 thinking 参数。
      </p>
    </Row>
  )

  return (
    <div className="space-y-4">
      <p className="text-xs leading-5 text-muted-foreground">
        每一项单独设置，"跟随默认"使用全局默认。注入的文本在
        <Link className="mx-1 text-primary underline-offset-2 hover:underline" to="/prompt-injection">提示词注入</Link>
        页面编辑。
      </p>
      {GROUPS.map((group) => (
        <section key={group.title} className="rounded-lg border bg-background" aria-labelledby={`route-group-${group.title}`}>
          <header className="border-b px-4 py-2.5">
            <h3 id={`route-group-${group.title}`} className="text-[13px] font-semibold">{group.title}</h3>
            <p className="text-[11px] leading-4 text-muted-foreground">{group.desc}</p>
          </header>
          <div className="divide-y px-4 py-3">
            {group.rows.map((row) =>
              row.kind === 'item'
                ? renderItem(row.id, row.note)
                : row.kind === 'feature'
                  ? renderFeature(row.key, row.note)
                  : renderThinkingTrigger())}
          </div>
        </section>
      ))}
    </div>
  )
}

/** Short badges for the route card. */
export function routeInjectionSummary(config: RuntimeConfig, catalog: InjectionCatalogEntry[], route: string): string[] {
  const working = config
  const enabled = catalog.filter((entry) => {
    const own = config.promptInjection.pathOverrides[route]?.[entry.id]?.enabled
    const value = own ?? inheritedItem(config.promptInjection, route, entry.id).enabled
    return value && !dependencyBlock(entry, working, route)
  }).length
  const features = effectiveFeatures(config.routeFeatures, route)
  const badges = [`注入 ${enabled}/${catalog.length}`]
  if (features.modelDrivenWebSearch) badges.push('WebSearch 模型驱动')
  if (features.channelLeakGuard) badges.push('渠道信息过滤')
  return badges
}
