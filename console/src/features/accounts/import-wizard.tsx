import { useMemo, useRef, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Check, ChevronDown, CircleAlert, FileUp, Loader2, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { credentialsApi } from '@/api/endpoints/credentials'
import type {
  AddCredentialRequest,
  BatchCredentialImportDefaults,
  BatchCredentialImportResponse,
  CredentialValidationItem,
} from '@/api/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { NumberInput, SelectControl } from '@/components/patterns/fields'
import { ToneBadge } from '@/components/status/tone-badge'
import { authMethodLabel, SUBSCRIPTION_LABEL, subscriptionTier } from '@/domain/labels'
import { dedupeCredentials, parseCredentialImportText } from '@/lib/credential-import'
import { cn } from '@/lib/utils'
import { qk } from '@/queries/keys'
import { useProxies } from '@/queries/shared'

type Step = 'source' | 'check' | 'defaults' | 'done'

interface Candidate {
  credential: AddCredentialRequest
  include: boolean
  check?: CredentialValidationItem
}

const STEPS: Array<{ key: Step; label: string }> = [
  { key: 'source', label: '来源' },
  { key: 'check', label: '预检' },
  { key: 'defaults', label: '导入参数' },
  { key: 'done', label: '结果' },
]

function credentialHint(c: AddCredentialRequest): string {
  if (c.email) return c.email
  if (c.kiroApiKey) return `${c.kiroApiKey.slice(0, 8)}…${c.kiroApiKey.slice(-4)}`
  if (c.refreshToken) return `refresh …${c.refreshToken.slice(-8)}`
  return '未知'
}

/**
 * 导入向导：替代原"添加账号 / 批量导入 / KAM 导入"三个入口。
 * 1) 粘贴或拖入文件，自动识别格式 2) 可选上游预检，剔除无效或重复 3) 统一默认参数后一次提交。
 */
export function ImportWizard({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const queryClient = useQueryClient()
  const proxies = useProxies()
  const [step, setStep] = useState<Step>('source')
  const [text, setText] = useState('')
  const [parseError, setParseError] = useState<string | null>(null)
  const [candidates, setCandidates] = useState<Candidate[]>([])
  const [dupes, setDupes] = useState(0)
  const [defaults, setDefaults] = useState<BatchCredentialImportDefaults>({ priority: 0, tags: [] })
  const [tagsText, setTagsText] = useState('')
  const [proxyMode, setProxyMode] = useState<'none' | 'single' | 'roundrobin'>('none')
  const [proxyIds, setProxyIds] = useState<number[]>([])
  const [duplicateMode, setDuplicateMode] = useState<'skip' | 'error'>('skip')
  const [autoDiscover, setAutoDiscover] = useState(false)
  const [result, setResult] = useState<BatchCredentialImportResponse | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)
  const [dragging, setDragging] = useState(false)

  const reset = () => {
    setStep('source')
    setText('')
    setParseError(null)
    setCandidates([])
    setDupes(0)
    setResult(null)
  }

  const close = (next: boolean) => {
    if (!next) {
      onOpenChange(false)
      setTimeout(reset, 200)
    }
  }

  const parse = (raw: string) => {
    setParseError(null)
    try {
      const parsed = parseCredentialImportText(raw)
      if (parsed.length === 0) {
        setParseError('没有识别到有效账号：需要 refreshToken（Social/IdC）或 ksk_ 开头的 API Key')
        return
      }
      const { unique, duplicates } = dedupeCredentials(parsed)
      setDupes(duplicates)
      setCandidates(unique.map((credential) => ({ credential, include: true })))
      setStep('check')
    } catch (e) {
      setParseError(`解析失败：${(e as Error).message}`)
    }
  }

  const readFiles = async (files: FileList | File[]) => {
    const contents = await Promise.all([...files].map((f) => f.text()))
    const merged = contents
      .map((c) => c.trim())
      .filter(Boolean)
      .map((c) => {
        try {
          const v = JSON.parse(c)
          return Array.isArray(v) ? v : [v]
        } catch {
          return null
        }
      })
    // 多文件时合并为 JSON 数组；若有非 JSON 文件则按文本拼接
    const combined = merged.every(Boolean) ? JSON.stringify(merged.flat()) : contents.join('\n')
    setText(combined)
    parse(combined)
  }

  const precheck = useMutation({
    mutationFn: () =>
      credentialsApi.validateExternal({
        credentials: candidates.map((c) => c.credential),
        querySubscription: true,
        queryUsage: true,
        checkLiveness: false,
      }),
    onSuccess: (res) => {
      const byIndex = new Map<number, CredentialValidationItem>()
      // 后端 index 从 1 开始
      res.groups.forEach((g) => g.items.forEach((item) => typeof item.index === 'number' && byIndex.set(item.index - 1, item)))
      setCandidates((list) =>
        list.map((c, i) => {
          const check = byIndex.get(i)
          return { ...c, check, include: check ? check.ok && !check.matchedExistingCredentialId : c.include }
        }),
      )
      toast.success(`预检完成：${res.success} 个可用，${res.failed} 个失败`)
    },
    meta: { error: '预检失败' },
  })

  const submit = useMutation({
    mutationFn: () => {
      const tags = tagsText.split(/[,，\n]/).map((t) => t.trim()).filter(Boolean)
      const payload: BatchCredentialImportDefaults = {
        ...defaults,
        tags,
        proxyResourceId: proxyMode === 'single' ? (proxyIds[0] ?? null) : undefined,
        proxyResourceIds: proxyMode === 'roundrobin' ? proxyIds : undefined,
      }
      return credentialsApi.import({
        credentials: candidates.filter((c) => c.include).map((c) => c.credential),
        defaults: payload,
        duplicateMode,
        continueOnError: true,
        autoDiscoverSupportedModels: autoDiscover,
      })
    },
    onSuccess: (res) => {
      setResult(res)
      setStep('done')
      queryClient.invalidateQueries({ queryKey: qk.credentials.all })
    },
    meta: { error: '导入失败' },
  })

  const included = candidates.filter((c) => c.include).length
  const stepIndex = STEPS.findIndex((s) => s.key === step)
  const proxyOptions = proxies.data?.resources.filter((p) => p.enabled) ?? []

  const counts = useMemo(() => {
    const checked = candidates.filter((c) => c.check)
    return {
      checked: checked.length,
      ok: checked.filter((c) => c.check?.ok).length,
      existing: checked.filter((c) => c.check?.matchedExistingCredentialId).length,
    }
  }, [candidates])

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="flex max-h-[90svh] flex-col gap-0 p-0 sm:max-w-3xl">
        <DialogHeader className="border-b p-5 pb-4">
          <DialogTitle>导入 Kiro 账号</DialogTitle>
          <DialogDescription>支持 JSON / JSONL、kiro-rs 导出备份、Kiro Account Manager 导出、ksk_ API Key 列表</DialogDescription>
          <ol className="mt-3 flex items-center gap-2 text-xs">
            {STEPS.map((s, i) => (
              <li key={s.key} className="flex items-center gap-2">
                <span
                  className={cn(
                    'flex size-5 items-center justify-center rounded-full border text-[11px] font-medium',
                    i < stepIndex && 'border-primary bg-primary text-primary-foreground',
                    i === stepIndex && 'border-primary text-primary',
                    i > stepIndex && 'text-muted-foreground',
                  )}
                >
                  {i < stepIndex ? <Check className="size-3" /> : i + 1}
                </span>
                <span className={i === stepIndex ? 'font-medium' : 'text-muted-foreground'}>{s.label}</span>
                {i < STEPS.length - 1 && <span className="h-px w-6 bg-border" />}
              </li>
            ))}
          </ol>
        </DialogHeader>

        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {step === 'source' && (
            <div className="space-y-3">
              <div
                onDragOver={(e) => {
                  e.preventDefault()
                  setDragging(true)
                }}
                onDragLeave={() => setDragging(false)}
                onDrop={(e) => {
                  e.preventDefault()
                  setDragging(false)
                  if (e.dataTransfer.files.length) void readFiles(e.dataTransfer.files)
                }}
                className={cn(
                  'flex flex-col items-center justify-center gap-2 rounded-xl border-2 border-dashed p-6 text-center transition-colors',
                  dragging ? 'border-primary bg-accent/40' : 'border-border',
                )}
              >
                <FileUp className="size-6 text-muted-foreground" />
                <p className="text-sm">拖入文件，或</p>
                <Button size="sm" variant="outline" onClick={() => fileRef.current?.click()}>
                  选择文件
                </Button>
                <input
                  ref={fileRef}
                  type="file"
                  multiple
                  accept=".json,.jsonl,.txt,application/json,text/plain"
                  className="hidden"
                  onChange={(e) => e.target.files && void readFiles(e.target.files)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="import-text">或粘贴内容</Label>
                <Textarea
                  id="import-text"
                  rows={10}
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  placeholder={'[{"refreshToken": "…", "authMethod": "social"}]\n或每行一个 ksk_xxx|us-east-1'}
                  className="font-mono text-xs"
                />
              </div>
              {parseError && (
                <p className="flex items-center gap-1.5 text-xs text-danger">
                  <CircleAlert className="size-3.5" /> {parseError}
                </p>
              )}
            </div>
          )}

          {step === 'check' && (
            <div className="space-y-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-sm">
                  识别到 <span className="num font-semibold">{candidates.length}</span> 个账号
                  {dupes > 0 && <span className="text-muted-foreground">（已合并 {dupes} 个重复项）</span>}
                  ，将导入 <span className="num font-semibold">{included}</span> 个
                </p>
                <Button size="sm" variant="outline" onClick={() => precheck.mutate()} disabled={precheck.isPending}>
                  {precheck.isPending && <Loader2 className="animate-spin" />}
                  上游预检（查询订阅与额度）
                </Button>
              </div>
              {counts.checked > 0 && (
                <p className="text-xs text-muted-foreground">
                  预检 {counts.checked} 个：可用 {counts.ok}，已存在 {counts.existing}。失败和已存在的账号已自动取消勾选。
                </p>
              )}
              <div className="divide-y rounded-lg border">
                {candidates.map((c, i) => {
                  const tier = subscriptionTier(c.check?.subscriptionTitle)
                  return (
                    <label key={i} className="flex cursor-pointer items-center gap-3 px-3 py-2 text-sm hover:bg-muted/40">
                      <Checkbox
                        checked={c.include}
                        onCheckedChange={(v) =>
                          setCandidates((list) => list.map((x, j) => (j === i ? { ...x, include: v === true } : x)))
                        }
                      />
                      <span className="num w-8 text-xs text-muted-foreground">{i + 1}</span>
                      <span className="min-w-0 flex-1 truncate">{credentialHint(c.credential)}</span>
                      <span className="text-xs text-muted-foreground">{authMethodLabel(c.credential.authMethod)}</span>
                      {c.check ? (
                        c.check.matchedExistingCredentialId ? (
                          <ToneBadge tone="neutral">已存在 #{c.check.matchedExistingCredentialId}</ToneBadge>
                        ) : c.check.ok ? (
                          <ToneBadge tone="success">{c.check.subscriptionTitle ? SUBSCRIPTION_LABEL[tier] : '可用'}</ToneBadge>
                        ) : (
                          <ToneBadge tone="danger" title={c.check.error ?? undefined}>
                            失败
                          </ToneBadge>
                        )
                      ) : (
                        <ToneBadge tone="neutral">未预检</ToneBadge>
                      )}
                    </label>
                  )
                })}
              </div>
            </div>
          )}

          {step === 'defaults' && (
            <div className="space-y-4">
              <p className="text-xs text-muted-foreground">以下参数只应用到未单独设置该字段的账号。</p>
              <div className="grid gap-4 sm:grid-cols-2">
                <div className="space-y-1.5">
                  <Label>优先级</Label>
                  <NumberInput value={defaults.priority ?? 0} min={0} onChange={(v) => setDefaults((d) => ({ ...d, priority: v ?? 0 }))} />
                </div>
                <div className="space-y-1.5">
                  <Label>最大并发</Label>
                  <NumberInput
                    value={defaults.maxConcurrentRequests ?? null}
                    allowEmpty
                    min={0}
                    placeholder="继承全局"
                    onChange={(v) => setDefaults((d) => ({ ...d, maxConcurrentRequests: v }))}
                  />
                </div>
                <div className="space-y-1.5 sm:col-span-2">
                  <Label htmlFor="import-tags">标签</Label>
                  <Input id="import-tags" value={tagsText} onChange={(e) => setTagsText(e.target.value)} placeholder="逗号分隔，例如 team-a, 生产" />
                </div>
                <div className="space-y-1.5 sm:col-span-2">
                  <Label>出站代理</Label>
                  <SelectControl
                    value={proxyMode}
                    onChange={(v) => {
                      setProxyMode(v)
                      setProxyIds([])
                    }}
                    options={[
                      { value: 'none', label: '不绑定（继承全局）' },
                      { value: 'single', label: '全部绑定同一个代理资源' },
                      { value: 'roundrobin', label: '按顺序轮流绑定多个代理资源' },
                    ]}
                  />
                  {proxyMode !== 'none' && (
                    <div className="flex flex-wrap gap-1.5 pt-1">
                      {proxyOptions.length === 0 && <span className="text-xs text-muted-foreground">没有可用的代理资源</span>}
                      {proxyOptions.map((p) => {
                        const on = proxyIds.includes(p.id)
                        return (
                          <Button
                            key={p.id}
                            size="xs"
                            variant={on ? 'default' : 'outline'}
                            onClick={() =>
                              setProxyIds((ids) =>
                                proxyMode === 'single' ? [p.id] : on ? ids.filter((x) => x !== p.id) : [...ids, p.id],
                              )
                            }
                          >
                            {p.name}
                          </Button>
                        )
                      })}
                    </div>
                  )}
                </div>
              </div>
              <Collapsible>
                <CollapsibleTrigger className="group flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
                  <ChevronDown className="size-3.5 transition-transform group-data-[state=open]:rotate-180" />
                  高级
                </CollapsibleTrigger>
                <CollapsibleContent className="mt-3 grid gap-4 sm:grid-cols-2">
                  <div className="space-y-1.5">
                    <Label>每分钟请求上限</Label>
                    <NumberInput value={defaults.rpm ?? null} allowEmpty min={0} placeholder="继承全局" onChange={(v) => setDefaults((d) => ({ ...d, rpm: v }))} />
                  </div>
                  <div className="space-y-1.5">
                    <Label>预热请求数</Label>
                    <NumberInput value={defaults.warmupRemaining ?? null} allowEmpty min={0} placeholder="使用全局" onChange={(v) => setDefaults((d) => ({ ...d, warmupRemaining: v ?? undefined }))} />
                  </div>
                  <div className="space-y-1.5">
                    <Label>重复账号</Label>
                    <SelectControl
                      value={duplicateMode}
                      onChange={setDuplicateMode}
                      options={[
                        { value: 'skip', label: '跳过已存在的账号' },
                        { value: 'error', label: '视为错误' },
                      ]}
                    />
                  </div>
                  <div className="space-y-2">
                    <ToggleLine label="导入后先保持禁用" checked={!!defaults.disabled} onChange={(v) => setDefaults((d) => ({ ...d, disabled: v }))} />
                    <ToggleLine label="导入后开启超额" checked={!!defaults.enableOverageAfterImport} onChange={(v) => setDefaults((d) => ({ ...d, enableOverageAfterImport: v }))} />
                    <ToggleLine label="自动发现支持的模型" checked={autoDiscover} onChange={setAutoDiscover} />
                  </div>
                </CollapsibleContent>
              </Collapsible>
            </div>
          )}

          {step === 'done' && result && (
            <div className="space-y-4">
              <div className="grid grid-cols-3 gap-3">
                <ResultStat label="成功" value={result.success} tone="text-success" />
                <ResultStat label="跳过" value={result.skipped} tone="text-muted-foreground" />
                <ResultStat label="失败" value={result.failed} tone="text-danger" />
              </div>
              {result.items.some((i) => !i.ok || i.warning) && (
                <div className="divide-y rounded-lg border text-xs">
                  {result.items
                    .filter((i) => !i.ok || i.warning)
                    .map((i) => (
                      <div key={i.index} className="flex gap-3 px-3 py-2">
                        <span className="num w-8 text-muted-foreground">{i.index + 1}</span>
                        <span className="min-w-0 flex-1 truncate">{i.email ?? (i.credentialId ? `#${i.credentialId}` : '')}</span>
                        <span className={i.ok ? 'text-warning' : i.skipped ? 'text-muted-foreground' : 'text-danger'}>
                          {i.error ?? i.warning ?? (i.skipped ? '已跳过' : '')}
                        </span>
                      </div>
                    ))}
                </div>
              )}
            </div>
          )}
        </div>

        <DialogFooter className="border-t p-4">
          {step === 'source' && (
            <Button onClick={() => parse(text)} disabled={!text.trim()}>
              识别
            </Button>
          )}
          {step === 'check' && (
            <>
              <Button variant="ghost" onClick={() => setStep('source')}>
                上一步
              </Button>
              <Button
                variant="ghost"
                onClick={() => setCandidates((list) => list.filter((c) => c.include))}
                disabled={included === candidates.length}
              >
                <Trash2 /> 移除未勾选
              </Button>
              <Button onClick={() => setStep('defaults')} disabled={included === 0}>
                下一步
              </Button>
            </>
          )}
          {step === 'defaults' && (
            <>
              <Button variant="ghost" onClick={() => setStep('check')}>
                上一步
              </Button>
              <Button onClick={() => submit.mutate()} disabled={submit.isPending || (proxyMode !== 'none' && proxyIds.length === 0)}>
                {submit.isPending && <Loader2 className="animate-spin" />}
                导入 {included} 个账号
              </Button>
            </>
          )}
          {step === 'done' && (
            <>
              <Button variant="ghost" onClick={reset}>
                继续导入
              </Button>
              <Button onClick={() => close(false)}>完成</Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ToggleLine({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="flex items-center justify-between gap-2 text-sm">
      {label}
      <Switch checked={checked} onCheckedChange={onChange} />
    </label>
  )
}

function ResultStat({ label, value, tone }: { label: string; value: number; tone: string }) {
  return (
    <div className="rounded-lg border p-3 text-center">
      <div className={cn('num text-2xl font-semibold', tone)}>{value}</div>
      <div className="text-xs text-muted-foreground">{label}</div>
    </div>
  )
}
