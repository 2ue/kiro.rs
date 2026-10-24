import * as React from 'react'
import { Link } from 'react-router-dom'
import { Check, PowerOff, RotateCcw } from 'lucide-react'
import { toast } from 'sonner'
import { Badge, Button, Switch, Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui'
import { Callout, ErrorState, LoadingState, PageContainer, PageHeader, useConfirm } from '@/components/patterns'
import { usePromptInjectionCatalog, useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import { normalizePromptInjection, normalizeRouteFeatures } from '@/lib/runtime-config-defaults'
import type { InjectionCatalogEntry, PromptInjectionConfig, RuntimeConfig } from '@/types/api'
import { InjectionItemCard } from './injection-item-card'
import { InjectionPreviewPanel } from './injection-preview-panel'
import { CATEGORY_LABEL, dependencyBlock, effectiveItem, knownRoutes } from './injection-model'

function textTotal(config: PromptInjectionConfig, catalog: InjectionCatalogEntry[]): number {
  return catalog
    .flatMap((entry) => entry.texts)
    .reduce((sum, variant) => sum + (config.texts[variant.key] ?? variant.defaultText).length, 0)
}

function RouteMatrix({ config, catalog }: { config: RuntimeConfig; catalog: InjectionCatalogEntry[] }) {
  const routes = knownRoutes(config)
  return (
    <div className="overflow-x-auto rounded-xl border bg-card">
      <table className="w-full min-w-[56rem] text-xs">
        <caption className="sr-only">各路由的注入项生效情况</caption>
        <thead>
          <tr className="text-left text-muted-foreground">
            <th scope="col" className="sticky left-0 bg-card p-2 font-medium">路由</th>
            {catalog.map((entry) => (
              <th key={entry.id} scope="col" className="p-2 font-medium" title={entry.name}>{entry.code}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {routes.map((route) => (
            <tr key={route} className="border-t">
              <th scope="row" className="sticky left-0 bg-card p-2 text-left font-mono font-medium">
                <Link className="text-primary underline-offset-2 hover:underline" to={`/cache-policies?route=${encodeURIComponent(route)}&tab=behavior`}>
                  {route}
                </Link>
              </th>
              {catalog.map((entry) => {
                const item = effectiveItem(config.promptInjection, route, entry.id)
                const blocked = item.enabled ? dependencyBlock(entry, config, route) : null
                const explicit = item.source.kind === 'route'
                const label = blocked ? '不生效' : item.enabled ? '开' : '关'
                const tone = blocked ? 'warning' : item.enabled ? 'success' : 'neutral'
                return (
                  <td key={entry.id} className="p-2" title={blocked ?? (explicit ? `路由 ${item.source.kind === 'route' ? item.source.prefix : ''} 显式设置` : '跟随默认')}>
                    <Badge size="xs" tone={tone}>{label}{explicit ? '·' : ''}</Badge>
                  </td>
                )
              })}
            </tr>
          ))}
        </tbody>
      </table>
      <p className="border-t p-2 text-[0.7rem] text-muted-foreground">
        带"·"的是路由上显式设置，其余跟随默认。点路由名进入该路由的编辑弹窗。
        {catalog.map((entry) => ` ${entry.code} ${entry.name}`).join('；')}
      </p>
    </div>
  )
}

export function PromptInjectionPage() {
  const runtime = useRuntimeConfig()
  const catalog = usePromptInjectionCatalog()
  const update = useUpdateRuntimeConfig()
  const confirm = useConfirm()
  const [draft, setDraft] = React.useState<PromptInjectionConfig | null>(null)
  const [applyToCountTokens, setApplyToCountTokens] = React.useState<boolean | null>(null)

  const saved = runtime.data
  React.useEffect(() => {
    if (saved) {
      setDraft(normalizePromptInjection(saved.promptInjection))
      setApplyToCountTokens(saved.promptInjection.applyToCountTokens)
    }
  }, [saved])

  if (runtime.isLoading || catalog.isLoading || !draft || !saved || !catalog.data) {
    if (runtime.isError || catalog.isError) {
      return <PageContainer><ErrorState message={extractErrorMessage(runtime.error ?? catalog.error)} /></PageContainer>
    }
    return <PageContainer><LoadingState /></PageContainer>
  }

  const entries = catalog.data.items
  const working: PromptInjectionConfig = { ...draft, applyToCountTokens: applyToCountTokens ?? draft.applyToCountTokens }
  const workingConfig: RuntimeConfig = { ...saved, promptInjection: working }
  const dirty = JSON.stringify(normalizePromptInjection(working)) !== JSON.stringify(normalizePromptInjection(saved.promptInjection))
  const total = textTotal(working, entries)
  const overLimit = total > catalog.data.maxTotalTextChars
  const prefixTextChanged = (['taskQuality', 'languageConstraint', 'custom', 'identityPrompt'] as const)
    .some((key) => (working.texts[key] ?? null) !== (saved.promptInjection.texts[key] ?? null))

  const save = async () => {
    if (prefixTextChanged) {
      const ok = await confirm({
        title: '保存注入文本',
        message: '任务质量、语言约束、自定义或身份声明的文本有改动。这几段计入缓存前缀，保存后所有会话的缓存前缀会失效一次。',
        confirmText: '保存',
      })
      if (!ok) return
    }
    try {
      await update.mutateAsync({
        ...saved,
        promptInjection: normalizePromptInjection(working),
        routeFeatures: normalizeRouteFeatures(saved.routeFeatures),
      })
      toast.success('提示词注入已保存，新请求立即生效')
    } catch (error) {
      toast.error(`保存失败：${extractErrorMessage(error)}`)
    }
  }

  const setAllDefaults = async (mode: 'off' | 'fresh') => {
    const ok = await confirm({
      title: mode === 'off' ? '全部默认值设为关' : '全部恢复新装默认',
      message: mode === 'off'
        ? '所有注入项的全局默认改为关闭。路由上的显式设置不变；标"建议开"的项关闭后会有报错或功能退化。'
        : '所有注入项的全局默认恢复成新装值。路由上的显式设置不变。',
      confirmText: '确认',
      tone: mode === 'off' ? 'danger' : 'default',
    })
    if (!ok) return
    const defaults = { ...draft.defaults }
    for (const entry of entries) {
      defaults[entry.id] = {
        ...defaults[entry.id],
        enabled: mode === 'off' ? false : entry.defaultEnabled,
      }
    }
    setDraft({ ...draft, defaults })
  }

  const groups = (['operator', 'protocol', 'feature', 'recovery'] as const).map((category) => ({
    category,
    items: entries.filter((entry) => entry.category === category),
  }))

  return (
    <PageContainer className="pb-24">
      <PageHeader
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => void setAllDefaults('fresh')}>
              <RotateCcw className="size-4" />全部恢复新装默认
            </Button>
            <Button variant="outline" size="sm" onClick={() => void setAllDefaults('off')}>
              <PowerOff className="size-4" />全部默认值设为关
            </Button>
            <Button size="sm" onClick={() => void save()} disabled={!dirty || overLimit || update.isPending}>
              <Check className="size-4" />保存
            </Button>
          </>
        }
      />
      <Callout tone="info">
        这里设置每个注入项的全局默认和注入文本。和路由的关联在
        <Link className="mx-1 underline underline-offset-2" to="/cache-policies">路由策略</Link>
        上逐项设置"跟随默认 / 开 / 关"。注入项之间互不影响，只有内容依附或属于某个功能的项会标出依赖。
      </Callout>
      {saved.compatProfile === 'anthropic-strict' && (
        <Callout tone="warning">当前 compat profile 是 anthropic-strict，标"strict 下不生效"的项都不会注入。</Callout>
      )}
      {overLimit && (
        <Callout tone="error">可编辑文本合计 {total} 字符，超过上限 {catalog.data.maxTotalTextChars}，无法保存。</Callout>
      )}

      <Tabs defaultValue="items">
        <TabsList>
          <TabsTrigger value="items">注入项</TabsTrigger>
          <TabsTrigger value="matrix">路由矩阵</TabsTrigger>
          <TabsTrigger value="fixed">不可关闭的内容</TabsTrigger>
          <TabsTrigger value="preview">预览</TabsTrigger>
        </TabsList>

        <TabsContent value="items" className="space-y-6">
          <label className="flex items-center justify-between gap-3 rounded-xl border bg-card p-4 text-sm" htmlFor="apply-count-tokens">
            <span>
              <span className="font-medium">count_tokens 计入注入内容</span>
              <span className="mt-0.5 block text-xs text-muted-foreground">
                计数时同样加上写进 system 的项（任务质量、语言约束、自定义、身份声明），避免估算低于真实请求。
              </span>
            </span>
            <Switch id="apply-count-tokens" checked={working.applyToCountTokens} onCheckedChange={setApplyToCountTokens} />
          </label>
          {groups.map(({ category, items }) => (
            <section key={category} className="space-y-3" aria-label={CATEGORY_LABEL[category]}>
              <h2 className="text-sm font-semibold">{CATEGORY_LABEL[category]}</h2>
              {items.map((entry) => (
                <InjectionItemCard key={entry.id} entry={entry} config={working} onChange={setDraft} />
              ))}
            </section>
          ))}
        </TabsContent>

        <TabsContent value="matrix">
          <RouteMatrix config={workingConfig} catalog={entries} />
        </TabsContent>

        <TabsContent value="fixed" className="space-y-3">
          {catalog.data.fixed.map((item) => (
            <article key={item.code} className="rounded-xl border bg-card p-4">
              <div className="flex items-center gap-2">
                <Badge size="xs" tone="neutral">{item.code}</Badge>
                <h3 className="text-sm font-semibold">{item.name}</h3>
              </div>
              <pre className="mt-2 whitespace-pre-wrap break-words rounded-md bg-muted/50 p-2 font-mono text-[0.7rem]">{item.text}</pre>
              <p className="mt-2 text-xs text-muted-foreground">没有开关的原因：{item.reason}</p>
            </article>
          ))}
        </TabsContent>

        <TabsContent value="preview">
          <InjectionPreviewPanel
            routes={knownRoutes(workingConfig)}
            catalog={entries}
            draft={{ promptInjection: normalizePromptInjection(working), routeFeatures: saved.routeFeatures }}
          />
        </TabsContent>
      </Tabs>
    </PageContainer>
  )
}
