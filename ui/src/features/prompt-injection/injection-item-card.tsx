import { AlertTriangle, RotateCcw } from 'lucide-react'
import { Badge, Button, Switch, Textarea } from '@/components/ui'
import type {
  InjectionCatalogEntry,
  InjectionTextKey,
  PromptInjectionConfig,
} from '@/types/api'
import { CATEGORY_LABEL } from './injection-model'
import { effectiveInjectionText, injectionCharCount, isInjectionTextEdited } from './injection-rules'

const RECOMMENDATION_LABEL = { keep_on: '建议开', user_choice: '由你决定' } as const

/** 一个注入项的全局默认开关与文本（与 main 提示词注入页的卡片一致）。 */
export function InjectionItemCard({
  entry,
  config,
  maxTextChars,
  onChange,
}: {
  entry: InjectionCatalogEntry
  config: PromptInjectionConfig
  /** 单段文本上限（字符），来自注入目录。 */
  maxTextChars: number
  onChange: (next: PromptInjectionConfig) => void
}) {
  const enabled = config.defaults[entry.id]?.enabled ?? entry.defaultEnabled
  const offWarning = entry.recommendation === 'keep_on' && !enabled
  const switchId = `injection-${entry.id}-enabled`

  const setEnabled = (next: boolean) =>
    onChange({ ...config, defaults: { ...config.defaults, [entry.id]: { enabled: next } } })
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
        <label className="flex shrink-0 items-center gap-2 text-xs font-medium" htmlFor={switchId}>
          默认开启
          <Switch id={switchId} checked={enabled} onCheckedChange={setEnabled} />
        </label>
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
            const value = config.texts[variant.key] ?? variant.defaultText
            const edited = isInjectionTextEdited(config.texts, variant)
            const chars = injectionCharCount(effectiveInjectionText(config.texts, variant))
            const over = chars > maxTextChars
            const textId = `injection-text-${variant.key}`
            return (
              <div key={variant.key} className="space-y-1.5">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <label htmlFor={textId} className="text-xs font-medium">
                    {entry.texts.length > 1 ? variant.label : '注入文本'}
                    {edited && <Badge size="xs" tone="primary" className="ml-2">已修改</Badge>}
                  </label>
                  {config.texts[variant.key] !== undefined && (
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
                  aria-invalid={over || undefined}
                  placeholder={variant.key === 'custom' ? '为空时不注入。文本计入缓存前缀，不要写时间或 request id。' : undefined}
                  onChange={(event) => {
                    const next = event.target.value
                    setText(variant.key, next === variant.defaultText ? null : next)
                  }}
                />
                <p className={over ? 'text-[0.7rem] text-destructive' : 'text-[0.7rem] text-muted-foreground'}>
                  {chars} 字符{over ? `，超过单段上限 ${maxTextChars}` : ''}
                  {variant.key !== 'custom' && '；清空后保存会恢复内置文本'}
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
