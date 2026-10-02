import { useMemo, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Check, CircleAlert, FileUp, Loader2, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { credentialsApi } from '@/api/endpoints/credentials'
import type {
  BatchCredentialImportDefaults,
  BatchCredentialImportItem,
  BatchCredentialImportResponse,
  CredentialValidationItem,
} from '@/api/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { dedupeCredentials, parseCredentialImportText } from '@/lib/credential-import'
import { cn } from '@/lib/utils'
import { qk } from '@/queries/keys'
import { useProxies } from '@/queries/shared'
import { ImportDefaults, ImportResult, type ProxyMode } from './import-steps'
import { ImportProgress } from './import-progress'
import { CandidateList, type Candidate } from './import-candidates'
import { useChunkedRunner } from './import-runner'

type Step = 'source' | 'check' | 'defaults' | 'done'

const STEPS: Array<{ key: Step; label: string }> = [
  { key: 'source', label: '来源' },
  { key: 'check', label: '预检' },
  { key: 'defaults', label: '导入参数' },
  { key: 'done', label: '结果' },
]

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
  const [proxyMode, setProxyMode] = useState<ProxyMode>('none')
  const [proxyIds, setProxyIds] = useState<number[]>([])
  const [duplicateMode, setDuplicateMode] = useState<'skip' | 'error'>('skip')
  const [autoDiscover, setAutoDiscover] = useState(false)
  const [result, setResult] = useState<(BatchCredentialImportResponse & { stopped?: boolean }) | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)
  const [dragging, setDragging] = useState(false)

  const reset = () => {
    setStep('source')
    setText('')
    setParseError(null)
    setCandidates([])
    setDupes(0)
    setResult(null)
    precheckRunner.reset()
    importRunner.reset()
  }

  const close = (next: boolean) => {
    if (!next && busy) {
      toast.info('正在处理，请先停止或等待完成')
      return
    }
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

  // 预检与导入都按批提交：后端逐个账号访问上游（每个约 1~3 秒），分批后可以实时显示进度
  const precheckRunner = useChunkedRunner<number, CredentialValidationItem>({ chunkSize: 5, concurrency: 2 })
  const importRunner = useChunkedRunner<Candidate, BatchCredentialImportItem>({ chunkSize: 3, concurrency: 2 })
  const busy = precheckRunner.running || importRunner.running

  const runPrecheck = async () => {
    const indexes = candidates.map((_, i) => i)
    const { items, errors, stopped } = await precheckRunner.run(indexes, async (chunk, offset) => {
      const res = await credentialsApi.validateExternal({
        credentials: chunk.map((i) => candidates[i]!.credential),
        querySubscription: true,
        queryUsage: true,
        checkLiveness: false,
      })
      // 后端 index 是本批内从 1 开始的序号，换算回全局下标
      const items = res.groups
        .flatMap((g) => g.items)
        .map((item) => ({ ...item, index: typeof item.index === 'number' ? offset + item.index - 1 : item.index }))
      setCandidates((list) =>
        list.map((c, i) => {
          const check = items.find((x) => x.index === i)
          return check ? { ...c, check, include: check.ok && !check.matchedExistingCredentialId } : c
        }),
      )
      return { items, ok: res.success, failed: res.failed, skipped: 0 }
    })
    const ok = items.filter((i) => i.ok).length
    if (errors.length)
      toast.warning(`预检${stopped ? '已停止' : '完成'}：${ok} 个可用，${errors.reduce((n, e) => n + e.count, 0)} 个因请求失败未检查`)
    else toast.success(`预检${stopped ? '已停止' : '完成'}：${ok} 个可用，${items.length - ok} 个失败`)
  }

  const runImport = async () => {
    const tags = tagsText
      .split(/[,，\n]/)
      .map((t) => t.trim())
      .filter(Boolean)
    const defaultsPayload: BatchCredentialImportDefaults = {
      ...defaults,
      tags,
      proxyResourceId: proxyMode === 'single' ? (proxyIds[0] ?? null) : undefined,
    }
    const selected = candidates.filter((c) => c.include)
    const { items, errors, stopped, progress } = await importRunner.run(selected, async (chunk, offset) => {
      const res = await credentialsApi.import({
        // 轮流绑定代理按全局顺序预先分配，避免分批后每批都从第一个代理开始
        credentials: chunk.map((c, i) =>
          proxyMode === 'roundrobin' && proxyIds.length && c.credential.proxyResourceId == null && !c.credential.proxyUrl
            ? { ...c.credential, proxyResourceId: proxyIds[(offset + i) % proxyIds.length] }
            : c.credential,
        ),
        defaults: defaultsPayload,
        duplicateMode,
        continueOnError: true,
        autoDiscoverSupportedModels: autoDiscover,
      })
      return {
        items: res.items.map((item) => ({ ...item, index: offset + item.index - 1 })),
        ok: res.success,
        failed: res.failed,
        skipped: res.skipped,
      }
    })
    // 整批请求失败的账号也要出现在结果里
    const failedChunks = errors.flatMap((e) =>
      Array.from({ length: e.count }, (_, k) => ({
        index: e.offset + k,
        ok: false,
        skipped: false,
        email: selected[e.offset + k]?.credential.email,
        error: (e.error as Error)?.message ?? '请求失败',
      })),
    )
    const all = [...items, ...failedChunks].sort((a, b) => a.index - b.index)
    setResult({ total: selected.length, success: progress.ok, skipped: progress.skipped, failed: progress.failed, items: all, stopped })
    setStep('done')
    queryClient.invalidateQueries({ queryKey: qk.credentials.all })
  }

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
                    'flex size-5 items-center justify-center rounded-full border text-2xs font-medium',
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
                {/* ui-rules-allow: 文件选择必须使用原生 input */}
                <input // ui-rules-allow: file input
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
                {!precheckRunner.running && (
                  <Button size="sm" variant="outline" onClick={() => void runPrecheck()} disabled={busy}>
                    上游预检（查询订阅与额度）
                  </Button>
                )}
              </div>
              {precheckRunner.progress && (precheckRunner.running || precheckRunner.progress.done < precheckRunner.progress.total) && (
                <ImportProgress
                  label="预检"
                  progress={precheckRunner.progress}
                  running={precheckRunner.running}
                  onStop={precheckRunner.stop}
                />
              )}
              {counts.checked > 0 && (
                <p className="text-xs text-muted-foreground">
                  预检 {counts.checked} 个：可用 {counts.ok}，已存在 {counts.existing}。失败和已存在的账号已自动取消勾选。
                </p>
              )}
              <CandidateList
                candidates={candidates}
                onToggle={(i, include) => setCandidates((list) => list.map((x, j) => (j === i ? { ...x, include } : x)))}
              />
            </div>
          )}

          {step === 'defaults' && !importRunner.running && (
            <ImportDefaults
              {...{
                defaults,
                setDefaults,
                tagsText,
                setTagsText,
                proxyMode,
                setProxyMode,
                proxyIds,
                setProxyIds,
                duplicateMode,
                setDuplicateMode,
                autoDiscover,
                setAutoDiscover,
                proxyOptions,
              }}
            />
          )}

          {step === 'defaults' && importRunner.progress && (
            <ImportProgress
              label="导入"
              progress={importRunner.progress}
              running={importRunner.running}
              onStop={importRunner.stop}
              className="mt-4"
            />
          )}

          {step === 'done' && result && <ImportResult result={result} />}
        </div>

        <DialogFooter className="m-0 rounded-b-xl border-t p-4">
          {step === 'source' && (
            <Button onClick={() => parse(text)} disabled={!text.trim()}>
              识别
            </Button>
          )}
          {step === 'check' && (
            <>
              <Button variant="ghost" onClick={() => setStep('source')} disabled={busy}>
                上一步
              </Button>
              <Button
                variant="ghost"
                onClick={() => setCandidates((list) => list.filter((c) => c.include))}
                disabled={busy || included === candidates.length}
              >
                <Trash2 /> 移除未勾选
              </Button>
              <Button onClick={() => setStep('defaults')} disabled={busy || included === 0}>
                下一步
              </Button>
            </>
          )}
          {step === 'defaults' && (
            <>
              <Button variant="ghost" onClick={() => setStep('check')} disabled={busy}>
                上一步
              </Button>
              <Button onClick={() => void runImport()} disabled={busy || (proxyMode !== 'none' && proxyIds.length === 0)}>
                {importRunner.running && <Loader2 className="animate-spin" />}
                {importRunner.running ? '导入中…' : `导入 ${included} 个账号`}
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
