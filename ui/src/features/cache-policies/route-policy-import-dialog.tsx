import { useMemo, useRef, useState } from 'react'
import { ArrowRight, FileUp, X } from 'lucide-react'
import { ModalShell } from '@/components/patterns'
import { Badge, Button, Textarea } from '@/components/ui'
import { ruleSummary } from '@/features/routes/route-utils'
import { CACHE_STRATEGIES } from './route-cache-editor'
import {
  MAIN_ROUTE_POLICY_FORMAT,
  missingRuleAccountIds,
  parseRoutePolicyImport,
  ROUTE_POLICY_FORMAT,
  ROUTE_POLICY_MAX_BYTES,
  ROUTE_POLICY_VERSION,
  type RoutePolicyImport,
} from './route-policy-transfer'

export type ImportedRoutePolicy = Extract<RoutePolicyImport, { ok: true }>

const PLACEHOLDER = `{
  "format": "${ROUTE_POLICY_FORMAT}",
  "version": ${ROUTE_POLICY_VERSION},
  "cachePolicy": { "cacheType": "current_high_cache" },
  "reportedUsage": null,
  "promptInjection": {},
  "features": {},
  "accountRule": { "mode": "allow_all", "rules": [] },
  "modelRule": { "mode": "allow_all", "rules": [] }
}`

const INJECTION_LABELS: Record<string, string> = {
  taskQuality: '任务质量',
  languageConstraint: '语言约束',
  custom: '自定义',
  identityPrompt: '身份声明',
}
const FEATURE_LABELS: Record<string, string> = {
  missingMaxTokens: '补全 max_tokens',
  reasoningCompat: '参数兼容',
  historySanitize: '历史清洗',
  thinkingModelName: '-thinking 模型名',
  thinkingTrigger: 'thinking 触发',
  payloadGuard: '请求体大小保护',
  payloadShaping: '请求体内容处理',
  imageProcessing: '图片处理',
  returnRequestModel: '返回请求模型',
}

function PreviewRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-x-3 gap-y-0.5 py-1.5 sm:grid-cols-[7rem_minmax(0,1fr)]">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  )
}

function ImportPreview({ imported, accountIds }: { imported: ImportedRoutePolicy; accountIds: readonly number[] }) {
  const { policy } = imported
  const strategy = CACHE_STRATEGIES.find(([key]) => key === (policy.cachePolicy.cacheType ?? 'current_high_cache'))?.[1] ?? '本地模拟缓存'
  const injections = Object.entries(policy.promptInjection)
  const features = Object.keys(policy.features)
  const missing = missingRuleAccountIds(policy.accountRule, accountIds)
  return (
    <section className="rounded-lg border bg-background px-4 py-3 text-xs" aria-label="导入预览">
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <span className="text-[13px] font-semibold">将导入的内容</span>
        <Badge size="xs" tone="neutral">
          {imported.origin === 'main' ? `main（kiro-rs）导出 · 版本 ${imported.version}` : `本项目导出 · 版本 ${imported.version}`}
        </Badge>
        {imported.sourceRoute && <Badge size="xs" tone="neutral">来自 {imported.sourceRoute}</Badge>}
      </div>
      {imported.highlights.length > 0 && (
        <div className="my-2 rounded-md bg-warning/10 px-3 py-2 text-warning" role="alert">
          <p className="font-medium">导入后行为会和原实例不同：</p>
          <ul className="mt-1 list-disc space-y-0.5 pl-4">
            {imported.highlights.map((item) => <li key={item}>{item}</li>)}
          </ul>
          <p className="mt-1">不需要的话，创建后在路由的“注入”页签里关掉。</p>
        </div>
      )}
      <dl className="divide-y">
        <PreviewRow label="缓存策略">{strategy}</PreviewRow>
        <PreviewRow label="下游 usage 整形">
          {policy.reportedUsage || policy.cachePolicy.reportedUsage ? '有路由单独设置' : '跟随模板'}
        </PreviewRow>
        <PreviewRow label="提示词注入">
          {injections.length === 0
            ? '全部跟随默认'
            : injections.map(([id, item]) => `${INJECTION_LABELS[id] ?? id}${item?.enabled ? '开' : '关'}`).join('、')}
        </PreviewRow>
        <PreviewRow label="路由功能">
          {features.length === 0 ? '全部跟随默认' : `显式设置：${features.map((key) => FEATURE_LABELS[key] ?? key).join('、')}`}
        </PreviewRow>
        <PreviewRow label="账号规则">
          {ruleSummary(policy.accountRule.mode, policy.accountRule.rules)}
          {missing.length > 0 && (
            <span className="block text-warning">当前实例没有账号 {missing.map((id) => `#${id}`).join('、')}，创建后请在“账号规则”里核对。</span>
          )}
        </PreviewRow>
        <PreviewRow label="模型规则">{ruleSummary(policy.modelRule.mode, policy.modelRule.rules)}</PreviewRow>
        {imported.ignored.length > 0 && (
          <PreviewRow label="已忽略">
            <span className="text-muted-foreground">本项目没有对应设置，不会导入：</span>
            <ul className="mt-1 list-disc space-y-0.5 pl-4 font-mono text-[0.7rem]">
              {imported.ignored.map((item) => <li key={item}>{item}</li>)}
            </ul>
          </PreviewRow>
        )}
      </dl>
    </section>
  )
}

/** 导入路由策略：粘贴 JSON 或选择文件，校验并预览，确认后交给新建路由流程。 */
export function RoutePolicyImportDialog({
  accountIds,
  onClose,
  onImport,
}: {
  /** 当前实例的账号 ID，用来提示账号规则里不存在的账号。 */
  accountIds: readonly number[]
  onClose: () => void
  onImport: (imported: ImportedRoutePolicy) => void
}) {
  const [text, setText] = useState('')
  const [fileName, setFileName] = useState<string | null>(null)
  const [fileError, setFileError] = useState<string | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const parsed = useMemo(() => (text.trim() ? parseRoutePolicyImport(text) : null), [text])
  const error = fileError ?? (parsed && !parsed.ok ? parsed.error : null)

  const readFile = async (file: File) => {
    if (file.size > ROUTE_POLICY_MAX_BYTES) {
      setFileError('文件超过 1 MB，不是有效的路由策略文件')
      return
    }
    try {
      setText(await file.text())
      setFileName(file.name)
      setFileError(null)
    } catch (err) {
      setFileError(`读取文件失败：${err instanceof Error ? err.message : String(err)}`)
    }
  }

  return (
    <ModalShell
      open
      title="导入路由策略"
      description="粘贴路由卡片“导出 JSON”得到的内容，或选择 JSON 文件；也兼容 main（kiro-rs）导出的路由策略。确认后进入新建路由，路由前缀需要重新填写。"
      width="max-w-2xl"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}><X />取消</Button>
          <Button onClick={() => parsed?.ok && onImport(parsed)} disabled={!parsed?.ok || fileError !== null}>
            <ArrowRight />
            下一步
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" variant="outline" size="sm" onClick={() => fileInput.current?.click()}>
            <FileUp />
            选择 JSON 文件
          </Button>
          <span className="text-xs text-muted-foreground">{fileName ? `已读取：${fileName}` : '也可以直接把 JSON 粘贴到下面'}</span>
          <input
            ref={fileInput}
            type="file"
            accept=".json,application/json"
            className="hidden"
            aria-label="选择路由策略 JSON 文件"
            onChange={(event) => {
              const file = event.target.files?.[0]
              if (file) void readFile(file)
              event.target.value = ''
            }}
          />
        </div>
        <label className="block text-sm font-medium" htmlFor="route-policy-import-text">策略 JSON</label>
        <Textarea
          id="route-policy-import-text"
          className="min-h-60 font-mono text-xs"
          placeholder={PLACEHOLDER}
          value={text}
          aria-invalid={Boolean(error)}
          aria-describedby={error ? 'route-policy-import-error' : undefined}
          onChange={(event) => {
            setText(event.target.value)
            setFileName(null)
            setFileError(null)
          }}
        />
        {error && <p id="route-policy-import-error" role="alert" className="text-xs text-destructive">{error}</p>}
        {parsed?.ok && <ImportPreview imported={parsed} accountIds={accountIds} />}
        {!text.trim() && (
          <p className="text-[0.7rem] leading-4 text-muted-foreground">
            支持 "format": "{ROUTE_POLICY_FORMAT}"（本项目）与 "{MAIN_ROUTE_POLICY_FORMAT}"（main）；main 的 Kiro 专属字段会被忽略并在预览里列出。
          </p>
        )}
      </div>
    </ModalShell>
  )
}
