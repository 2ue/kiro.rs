import { AlertTriangle, RotateCcw } from 'lucide-react'
import { Badge, Button, Switch, Textarea } from '@/components/ui'
import type {
  InjectionCatalogEntry,
  InjectionTextKey,
  PromptInjectionConfig,
} from '@/types/api'
import { CATEGORY_LABEL } from './injection-model'

const RECOMMENDATION_LABEL = { keep_on: '建议开', user_choice: '由你决定' } as const

export function InjectionItemCard({
  entry,
  config,
  onChange,
}: {
  entry: InjectionCatalogEntry
  config: PromptInjectionConfig
  onChange: (next: PromptInjectionConfig) => void
}) {
  const setting = config.defaults[entry.id] ?? { enabled: entry.defaultEnabled }
  const offWarning = entry.recommendation === 'keep_on' && !setting.enabled
  const switchId = `injection-${entry.id}-enabled`
  const externalId = `injection-${entry.id}-external`

  const setSetting = (patch: Partial<typeof setting>) =>
    onChange({ ...config, defaults: { ...config.defaults, [entry.id]: { ...setting, ...patch } } })
  const setText = (key: InjectionTextKey, text: string | null) => {
    const texts = { ...config.texts }
    if (text === null) delete texts[key]
    else texts[key] = text
    onChange({ ...config, texts })
  }

  return (
    <article className="rounded-xl border bg-card p-4" aria-labelledby={`${switchId}-title`}>
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <Badge size="xs" tone="neutral">{entry.code}</Badge>
            <h3 id={`${switchId}-title`} className="text-sm font-semibold">{entry.name}</h3>
            <Badge size="xs" tone="info">{CATEGORY_LABEL[entry.category]}</Badge>
            <Badge size="xs" tone={entry.recommendation === 'keep_on' ? 'warning' : 'neutral'}>
              {RECOMMENDATION_LABEL[entry.recommendation]}
            </Badge>
            {entry.strictBlocks && <Badge size="xs" tone="neutral">strict 下不生效</Badge>}
          </div>
          <p className="text-xs leading-5 text-muted-foreground">
            位置：{entry.position}。触发：{entry.trigger}。
          </p>
        </div>
        <div className="flex shrink-0 flex-col items-end gap-2">
          <label className="flex items-center gap-2 text-xs font-medium" htmlFor={switchId}>
            默认开启
            <Switch id={switchId} checked={setting.enabled} onCheckedChange={(enabled) => setSetting({ enabled })} />
          </label>
          {entry.externalPool && (
            <label className="flex items-center gap-2 text-xs text-muted-foreground" htmlFor={externalId}>
              外部池也注入
              <Switch
                id={externalId}
                checked={setting.externalPool ?? false}
                disabled={!setting.enabled}
                onCheckedChange={(externalPool) => setSetting({ externalPool })}
              />
            </label>
          )}
        </div>
      </header>

      <dl className="mt-3 grid gap-2 text-xs leading-5 md:grid-cols-2">
        <div className={offWarning ? 'rounded-md border border-destructive/30 bg-destructive/5 p-2 text-destructive' : 'rounded-md bg-muted/40 p-2'}>
          <dt className="flex items-center gap-1 font-medium">
            {offWarning && <AlertTriangle className="size-3.5" aria-hidden />}
            关闭后的影响
          </dt>
          <dd className={offWarning ? '' : 'text-muted-foreground'}>{entry.impactWhenOff}</dd>
        </div>
        <div className="rounded-md bg-muted/40 p-2">
          <dt className="font-medium">建议</dt>
          <dd className="text-muted-foreground">{entry.recommendationNote}</dd>
        </div>
      </dl>

      {entry.texts.length > 0 && (
        <div className="mt-3 space-y-3">
          {entry.texts.map((variant) => {
            const edited = config.texts[variant.key]
            const value = edited ?? variant.defaultText
            const textId = `injection-text-${variant.key}`
            return (
              <div key={variant.key} className="space-y-1.5">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <label htmlFor={textId} className="text-xs font-medium">
                    {entry.texts.length > 1 ? variant.label : '注入文本'}
                    {edited !== undefined && <Badge size="xs" tone="primary" className="ml-2">已修改</Badge>}
                  </label>
                  {edited !== undefined && (
                    <Button variant="ghost" size="sm" onClick={() => setText(variant.key, null)}>
                      <RotateCcw className="size-3.5" />
                      恢复默认
                    </Button>
                  )}
                </div>
                <Textarea
                  id={textId}
                  className="min-h-24 font-mono text-xs"
                  value={value}
                  placeholder={variant.key === 'custom' ? '为空时不注入。文本计入缓存前缀，不要写时间或 request id。' : undefined}
                  onChange={(event) => {
                    const next = event.target.value
                    setText(variant.key, next === variant.defaultText ? null : next)
                  }}
                />
                <p className="text-[0.7rem] text-muted-foreground">
                  {value.length} 字符{variant.key !== 'custom' && '；清空后保存会恢复内置文本'}
                </p>
              </div>
            )
          })}
        </div>
      )}

      {entry.readOnlyVariants.length > 0 && (
        <div className="mt-3 space-y-2">
          <p className="text-xs text-muted-foreground">
            协议控制标签，只能开关不能编辑：上游按这些标签识别参数，输出清洗也要求和注入内容逐字一致。
          </p>
          {entry.readOnlyVariants.map((variant) => (
            <div key={variant.label}>
              <div className="text-[0.7rem] font-medium text-muted-foreground">{variant.label}</div>
              <pre className="mt-1 overflow-x-auto whitespace-pre-wrap break-all rounded-md bg-muted/50 p-2 font-mono text-[0.7rem]">
                {variant.defaultText}
              </pre>
            </div>
          ))}
        </div>
      )}
    </article>
  )
}
