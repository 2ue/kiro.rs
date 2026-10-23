import * as React from 'react'
import { Eye } from 'lucide-react'
import { Badge, Button, Switch } from '@/components/ui'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Callout, Field } from '@/components/patterns'
import { usePromptInjectionPreview } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type {
  InjectionCatalogEntry,
  PreviewThinking,
  PreviewToolChoice,
  PromptInjectionConfig,
  PromptInjectionPreviewRequest,
  RouteFeaturesConfig,
} from '@/types/api'
import { BLOCK_REASON_LABEL, THINKING_TRIGGER_LABEL } from './injection-model'

const STATUS_LABEL = { applied: '已注入', not_triggered: '未触发', blocked: '不注入' } as const
const STATUS_TONE = { applied: 'success', not_triggered: 'neutral', blocked: 'neutral' } as const

function TextBlock({ title, text }: { title: string; text?: string | null }) {
  if (!text) return null
  return (
    <div className="space-y-1">
      <div className="text-xs font-medium">{title}</div>
      <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/50 p-3 font-mono text-[0.7rem] leading-5">
        {text}
      </pre>
    </div>
  )
}

function BoolField({ id, label, checked, onChange }: { id: string; label: string; checked: boolean; onChange: (value: boolean) => void }) {
  return (
    <label htmlFor={id} className="flex items-center justify-between gap-3 rounded-md border px-3 py-2 text-xs">
      {label}
      <Switch id={id} checked={checked} onCheckedChange={onChange} />
    </label>
  )
}

export function InjectionPreviewPanel({
  routes,
  catalog,
  draft,
}: {
  routes: string[]
  catalog: InjectionCatalogEntry[]
  draft: { promptInjection: PromptInjectionConfig; routeFeatures: RouteFeaturesConfig }
}) {
  const preview = usePromptInjectionPreview()
  const [request, setRequest] = React.useState<PromptInjectionPreviewRequest>({
    route: routes.includes('/cc') ? '/cc' : routes[0] ?? '/v1',
    upstream: 'local',
    model: 'claude-sonnet-4-5',
    nativeReasoning: false,
    withSystem: true,
    withWriteEdit: true,
    toolChoice: 'auto',
    thinking: 'off',
    language: 'none',
    emptyTurnRetry: false,
    stream: false,
  })
  const set = <K extends keyof PromptInjectionPreviewRequest>(key: K) => (value: PromptInjectionPreviewRequest[K]) =>
    setRequest((current) => ({ ...current, [key]: value }))
  const names = new Map(catalog.map((entry) => [entry.id, `${entry.code} ${entry.name}`]))
  const result = preview.data

  return (
    <div className="grid gap-4 xl:grid-cols-[22rem_minmax(0,1fr)]">
      <form
        className="space-y-3 rounded-xl border bg-card p-4"
        aria-label="预览条件"
        onSubmit={(event) => {
          event.preventDefault()
          preview.mutate({ ...request, draft })
        }}
      >
        <Field label="路由" htmlFor="preview-route">
          <Select value={request.route} onValueChange={set('route')}>
            <SelectTrigger id="preview-route" size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              {routes.map((route) => <SelectItem key={route} value={route}>{route}</SelectItem>)}
            </SelectContent>
          </Select>
        </Field>
        <Field label="上游" htmlFor="preview-upstream">
          <Select value={request.upstream} onValueChange={(value) => set('upstream')(value as 'local' | 'external_pool')}>
            <SelectTrigger id="preview-upstream" size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="local">本地 Kiro</SelectItem>
              <SelectItem value="external_pool">外部池</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="tool_choice" htmlFor="preview-tool-choice">
          <Select value={request.toolChoice} onValueChange={(value) => set('toolChoice')(value as PreviewToolChoice)}>
            <SelectTrigger id="preview-tool-choice" size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">auto</SelectItem>
              <SelectItem value="any">any</SelectItem>
              <SelectItem value="tool">tool（Read）</SelectItem>
              <SelectItem value="none">none</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="thinking" htmlFor="preview-thinking">
          <Select value={request.thinking} onValueChange={(value) => set('thinking')(value as PreviewThinking)}>
            <SelectTrigger id="preview-thinking" size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="off">不带 thinking</SelectItem>
              <SelectItem value="enabled">enabled</SelectItem>
              <SelectItem value="adaptive">adaptive</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <div className="grid gap-2">
          <BoolField id="preview-system" label="请求带 system" checked={request.withSystem ?? true} onChange={set('withSystem')} />
          <BoolField id="preview-write" label="声明 Write/Edit 工具" checked={request.withWriteEdit ?? true} onChange={set('withWriteEdit')} />
          <BoolField id="preview-parallel" label="disable_parallel_tool_use" checked={request.disableParallelToolUse ?? false} onChange={set('disableParallelToolUse')} />
          <BoolField id="preview-native" label="模型有原生 reasoning 字段" checked={request.nativeReasoning ?? false} onChange={set('nativeReasoning')} />
          <BoolField
            id="preview-language"
            label="客户端声明了回复语言"
            checked={request.language === 'declared'}
            onChange={(value) => set('language')(value ? 'declared' : 'none')}
          />
          <BoolField id="preview-retry" label="同时展示空轮重试" checked={request.emptyTurnRetry ?? false} onChange={set('emptyTurnRetry')} />
          <BoolField id="preview-stream" label="流式请求" checked={request.stream ?? false} onChange={set('stream')} />
        </div>
        <Button type="submit" className="w-full" disabled={preview.isPending}>
          <Eye className="size-4" />
          生成预览
        </Button>
        <p className="text-[0.7rem] leading-4 text-muted-foreground">
          使用页面上还没保存的设置。后端走和真实请求相同的注入与转换代码，不访问上游，也不会保存任何东西。
        </p>
      </form>

      <section className="space-y-4 rounded-xl border bg-card p-4" aria-live="polite" aria-label="预览结果">
        {preview.isError && <Callout tone="error">预览失败：{extractErrorMessage(preview.error)}</Callout>}
        {!result && !preview.isError && (
          <p className="text-sm text-muted-foreground">选好条件后点"生成预览"。</p>
        )}
        {result && (
          <>
            <div className="text-xs text-muted-foreground">入口：<code>{result.endpoint}</code></div>
            {result.error && <Callout tone="error">这个请求会被拒绝：{result.error}</Callout>}
            <div className="flex flex-wrap gap-2 text-xs">
              <Badge tone="neutral">-thinking 模型名：{result.features.thinkingModelName ? '开' : '关'}</Badge>
              <Badge tone="neutral">thinking 触发：{THINKING_TRIGGER_LABEL[result.features.thinkingTrigger]}</Badge>
              <Badge tone="neutral">渠道信息过滤：{result.features.channelLeakGuard ? '开' : '关'}</Badge>
              <Badge tone="neutral">WebSearch 模型驱动：{result.features.modelDrivenWebSearch ? '开' : '关'}</Badge>
            </div>
            <div className="overflow-x-auto">
              <table className="w-full min-w-[32rem] text-left text-xs">
                <thead className="text-muted-foreground">
                  <tr><th className="py-1.5 pr-3 font-medium">注入项</th><th className="py-1.5 pr-3 font-medium">结果</th><th className="py-1.5 font-medium">原因 / 来源</th></tr>
                </thead>
                <tbody>
                  {result.items.map((item) => (
                    <tr key={item.id} className="border-t">
                      <td className="py-1.5 pr-3">{names.get(item.id) ?? item.id}</td>
                      <td className="py-1.5 pr-3"><Badge size="xs" tone={STATUS_TONE[item.status]}>{STATUS_LABEL[item.status]}</Badge></td>
                      <td className="py-1.5 text-muted-foreground">
                        {item.blocked ? BLOCK_REASON_LABEL[item.blocked] : item.status === 'not_triggered' ? '这个请求不满足触发条件' : ''}
                        {item.source ? `（路由 ${item.source} 设置）` : item.blocked === 'off' ? '（全局默认）' : ''}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {result.externalSystem && <TextBlock title="发往外部池的 system" text={result.externalSystem.join('\n\n---\n\n')} />}
            <TextBlock title="history 第一条（由 system 合成，后面跟一条 assistant：I will follow these instructions.）" text={result.systemTurn} />
            <TextBlock title="当前轮 user 消息" text={result.currentTurn} />
            {result.tools.length > 0 && (
              <TextBlock title="工具描述" text={result.tools.map(([name, description]) => `## ${name}\n${description}`).join('\n\n')} />
            )}
            <TextBlock title="空轮重试时的当前轮 user 消息" text={result.emptyTurnRetryTurn} />
          </>
        )}
      </section>
    </div>
  )
}
