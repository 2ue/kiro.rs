import { Link } from 'react-router-dom'
import { Badge } from '@/components/ui'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import type {
  InjectionCatalogEntry,
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

/**
 * One route's prompt-injection settings. `value` holds only the explicit settings of this
 * route; `config` is the saved runtime config used to show inherited values and dependencies.
 */
export function RouteInjectionEditor({
  route,
  catalog,
  config,
  value,
  onChange,
}: {
  route: string
  catalog: InjectionCatalogEntry[]
  config: RuntimeConfig
  value: Partial<Record<string, InjectionItemOverride>>
  onChange: (next: Partial<Record<string, InjectionItemOverride>>) => void
}) {
  const working: RuntimeConfig = {
    ...config,
    promptInjection: {
      ...config.promptInjection,
      pathOverrides: { ...config.promptInjection.pathOverrides, [route]: value },
    },
  }
  const set = (id: string, patch: InjectionItemOverride) => {
    const next = { ...value, [id]: { ...(value[id] ?? {}), ...patch } }
    const item = next[id]!
    if (item.enabled === undefined && item.externalPool === undefined) delete next[id]
    onChange(next)
  }

  return (
    <div className="space-y-3">
      <p className="text-xs leading-5 text-muted-foreground">
        每一项单独设置，"跟随默认"使用提示词注入页上的全局默认。注入的文本统一在
        <Link className="mx-1 text-primary underline-offset-2 hover:underline" to="/prompt-injection">提示词注入</Link>
        页面编辑。
      </p>
      <div className="grid gap-3 md:grid-cols-2">
        {catalog.map((entry) => {
          const own = value[entry.id] ?? {}
          const inherited = inheritedItem(config.promptInjection, route, entry.id)
          const effectiveEnabled = own.enabled ?? inherited.enabled
          const blocked = effectiveEnabled ? dependencyBlock(entry, working, route) : null
          const id = `route-injection-${entry.id}`
          const preview = entry.texts[0]
            ? (config.promptInjection.texts[entry.texts[0].key] ?? entry.texts[0].defaultText)
            : entry.readOnlyVariants[0]?.defaultText ?? ''
          const warnOff = entry.recommendation === 'keep_on' && !effectiveEnabled
          return (
            <div key={entry.id} className="min-w-0 space-y-1.5 rounded-md border bg-background p-3">
              <div className="flex flex-wrap items-center gap-1.5">
                <Badge size="xs" tone="neutral">{entry.code}</Badge>
                <label className="text-sm font-medium" htmlFor={id}>{entry.name}</label>
                {entry.recommendation === 'keep_on' && <Badge size="xs" tone="warning">建议开</Badge>}
              </div>
              <TriSelect id={id} value={own.enabled} inherited={inherited.enabled} onChange={(enabled) => set(entry.id, { enabled })} />
              {entry.externalPool && (
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground" htmlFor={`${id}-external`}>外部池也注入</label>
                  <TriSelect
                    id={`${id}-external`}
                    value={own.externalPool}
                    inherited={inherited.externalPool}
                    disabled={!effectiveEnabled}
                    onChange={(externalPool) => set(entry.id, { externalPool })}
                  />
                </div>
              )}
              {blocked && <p className="text-xs text-warning">不生效：{blocked}</p>}
              {warnOff && <p className="text-xs text-destructive">关闭后：{entry.impactWhenOff}</p>}
              {preview && (
                <p className="line-clamp-2 break-all font-mono text-[0.68rem] leading-4 text-muted-foreground" title={preview}>
                  {preview.slice(0, 160)}
                </p>
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}

const FEATURE_FIELDS: Array<{ key: keyof RouteFeatureSet; label: string; desc: string }> = [
  {
    key: 'thinkingModelName',
    label: '-thinking 模型名',
    desc: '请求 claude-xxx-thinking 这类模型名时，自动补上 thinking 参数。关闭后这类模型名按普通模型处理。',
  },
  {
    key: 'channelLeakGuard',
    label: '渠道信息输出过滤',
    desc: '删除回复里"我是 Kiro""指令冲突"这类暴露上游的句子。正则较宽，讨论 kiro.rs 代码时可能误删正常句子。',
  },
  {
    key: 'channelLeakGuardStream',
    label: '流式也过滤',
    desc: '流式回复按整句检查后再下发，首字会稍晚。',
  },
  {
    key: 'modelDrivenWebSearch',
    label: 'WebSearch 模型驱动',
    desc: '请求带原生 web_search 工具时，由模型决定搜索词并写带引用的回答；每次搜索多调用 1 次模型，token 计入本次 usage。关闭时沿用模板回答。',
  },
]

export function RouteFeaturesEditor({
  route,
  config,
  value,
  onChange,
}: {
  route: string
  config: RuntimeConfig
  value: RouteFeatureOverride
  onChange: (next: RouteFeatureOverride) => void
}) {
  const inherited = inheritedFeatures(config.routeFeatures, route)
  const effective = effectiveFeatures(
    { ...config.routeFeatures, pathOverrides: { ...config.routeFeatures.pathOverrides, [route]: value } },
    route,
  )
  const set = <K extends keyof RouteFeatureSet>(key: K, next: RouteFeatureSet[K] | undefined) => {
    const updated = { ...value }
    if (next === undefined) delete updated[key]
    else updated[key] = next
    onChange(updated)
  }

  return (
    <div className="grid gap-3 md:grid-cols-2">
      <div className="min-w-0 space-y-1.5 rounded-md border bg-background p-3">
        <label className="text-sm font-medium" htmlFor="route-feature-thinking-trigger">thinking 触发方式</label>
        <Select
          value={value.thinkingTrigger ?? 'inherit'}
          onValueChange={(raw) => set('thinkingTrigger', raw === 'inherit' ? undefined : (raw as ThinkingTriggerMode))}
        >
          <SelectTrigger id="route-feature-thinking-trigger" size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="inherit">{`跟随默认（当前${THINKING_TRIGGER_LABEL[inherited.thinkingTrigger]}）`}</SelectItem>
            <SelectItem value="real_request">按真实请求</SelectItem>
            <SelectItem value="always">总是开启</SelectItem>
            <SelectItem value="off">不自动开启</SelectItem>
          </SelectContent>
        </Select>
        <p className="text-xs leading-5 text-muted-foreground">
          按真实请求：用户消息里有 Claude Code 的深度思考信号（如 ultrathink）时开启并强制可见。总是开启：请求没有关闭 thinking 时都开启。不自动开启：只看客户端自己的 thinking 参数。
        </p>
      </div>
      {FEATURE_FIELDS.map((field) => {
        const id = `route-feature-${field.key}`
        const disabled = field.key === 'channelLeakGuardStream' && !effective.channelLeakGuard
        return (
          <div key={field.key} className="min-w-0 space-y-1.5 rounded-md border bg-background p-3">
            <label className="text-sm font-medium" htmlFor={id}>{field.label}</label>
            <TriSelect
              id={id}
              value={value[field.key] as boolean | undefined}
              inherited={inherited[field.key] as boolean}
              disabled={disabled}
              onChange={(next) => set(field.key, next as never)}
            />
            <p className="text-xs leading-5 text-muted-foreground">{field.desc}</p>
          </div>
        )
      })}
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
