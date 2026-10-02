import { useMemo, useRef, useState } from 'react'
import { Check, CircleAlert, FileUp } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { authMethodLabel } from '@/domain/labels'
import { parseCredentialImportEntries, type CredentialImportEntry } from '@/lib/credential-import'
import { cn } from '@/lib/utils'
import { useModelCapabilities, useProxies } from '@/queries/shared'
import { useAccountsPage } from './queries'
import { initialDefaults, initialOptions, validateDefaults } from './import/defaults'
import { ImportProgress } from './import/import-progress'
import { ImportSettings } from './import/import-settings'
import type { QueueItem } from './import/queue'
import { useImportQueue } from './import/use-import-queue'

type Step = 'source' | 'settings' | 'progress'

const STEPS: Array<{ key: Step; label: string }> = [
  { key: 'source', label: '来源' },
  { key: 'settings', label: '导入参数' },
  { key: 'progress', label: '导入进度' },
]

const FALLBACK_TEST_MODEL = 'claude-sonnet-4.5'

function entryLabel(e: CredentialImportEntry, i: number): string {
  const c = e.credential
  if (c.email) return c.email
  if (c.kiroApiKey) return `${c.kiroApiKey.slice(0, 8)}…${c.kiroApiKey.slice(-4)}`
  if (c.refreshToken) return `${authMethodLabel(c.authMethod)} · …${c.refreshToken.slice(-8)}`
  return `账号 ${i + 1}`
}

/**
 * 导入向导：合并原"批量导入 / KAM 导入"。
 * 1) 粘贴或拖入文件，自动识别 JSON / JSONL / KAM / ksk_ 列表
 * 2) 导入参数（与旧版字段一致）
 * 3) 逐个导入并验活，实时显示每个账号的状态，失败可单个或批量重试
 */
export function ImportWizard({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const proxies = useProxies()
  const capabilities = useModelCapabilities()
  const existing = useAccountsPage({ size: 200 })
  const [step, setStep] = useState<Step>('source')
  const [text, setText] = useState('')
  const [parseError, setParseError] = useState<string | null>(null)
  const [entries, setEntries] = useState<CredentialImportEntry[]>([])
  const [dupes, setDupes] = useState(0)
  const [defaults, setDefaults] = useState(initialDefaults)
  const [options, setOptions] = useState(initialOptions)
  const fileRef = useRef<HTMLInputElement>(null)
  const [dragging, setDragging] = useState(false)

  const testModel = useMemo(() => {
    const models = capabilities.data?.models.map((m) => m.model).filter((m) => m !== 'auto' && !m.endsWith('-thinking')) ?? []
    return models.find((m) => /sonnet-4[.-]5/.test(m)) ?? models[0] ?? FALLBACK_TEST_MODEL
  }, [capabilities.data])
  const queue = useImportQueue({ defaults, options, testModel })

  const tagOptions = useMemo(() => [...new Set(existing.rows.flatMap((r) => r.tags ?? []))].sort(), [existing.rows])
  const proxyOptions = proxies.data?.resources.filter((p) => p.enabled) ?? []
  const errorCount = entries.filter((e) => e.sourceStatus === 'error').length

  const reset = () => {
    setStep('source')
    setText('')
    setParseError(null)
    setEntries([])
    setDupes(0)
    setDefaults(initialDefaults())
    setOptions(initialOptions())
    queue.reset()
  }

  const close = (next: boolean) => {
    if (next) return
    if (queue.running) {
      toast.info('正在导入，请先停止或等待完成')
      return
    }
    onOpenChange(false)
    setTimeout(reset, 200)
  }

  const parse = (raw: string) => {
    setParseError(null)
    try {
      const { entries: parsed, duplicates } = parseCredentialImportEntries(raw)
      if (!parsed.length) {
        setParseError('没有识别到有效账号：需要 refreshToken（Social/IdC）或 ksk_ 开头的 API Key')
        return
      }
      setEntries(parsed)
      setDupes(duplicates)
      setStep('settings')
    } catch (e) {
      setParseError(`解析失败：${(e as Error).message}`)
    }
  }

  const readFiles = async (files: FileList | File[]) => {
    const contents = await Promise.all([...files].map((f) => f.text()))
    const asJson = contents.map((c) => {
      try {
        const v = JSON.parse(c.trim())
        return Array.isArray(v) ? v : [v]
      } catch {
        return null
      }
    })
    // 多个 JSON 文件合并为一个数组；有非 JSON 文件时按文本拼接
    const combined = asJson.every(Boolean) ? JSON.stringify(asJson.flat()) : contents.join('\n')
    setText(combined)
    parse(combined)
  }

  const start = () => {
    const error = validateDefaults(defaults)
    if (error) {
      toast.error(error)
      return
    }
    const items: QueueItem[] = entries.map((e, i) => ({
      index: i,
      label: entryLabel(e, i),
      email: e.credential.email,
      credential: e.credential,
      sourceStatus: e.sourceStatus,
      status: 'pending',
      attempts: 0,
    }))
    setStep('progress')
    void queue.start(items)
  }

  const stepIndex = STEPS.findIndex((st) => st.key === step)
  const s = queue.summary

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="flex max-h-[92svh] flex-col gap-0 p-0 sm:max-w-3xl">
        <DialogHeader className="border-b p-5 pb-4">
          <DialogTitle>导入 Kiro 账号</DialogTitle>
          <DialogDescription>支持 JSON / JSONL、kiro-rs 导出备份、Kiro Account Manager（KAM）导出、ksk_ API Key 列表</DialogDescription>
          <ol className="mt-3 flex items-center gap-2 text-xs">
            {STEPS.map((st, i) => (
              <li key={st.key} className="flex items-center gap-2">
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
                <span className={i === stepIndex ? 'font-medium' : 'text-muted-foreground'}>{st.label}</span>
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
                <input // ui-rules-allow: 文件选择必须使用原生 input
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

          {step === 'settings' && (
            <div className="space-y-4">
              <p className="text-sm">
                识别到 <span className="num font-semibold">{entries.length}</span> 个账号
                {dupes > 0 && <span className="text-muted-foreground">（已合并 {dupes} 个重复项）</span>}
                {errorCount > 0 && <span className="text-muted-foreground">，其中 {errorCount} 个在来源中标记为 error</span>}
                。已存在的账号会自动跳过。
              </p>
              <ImportSettings
                defaults={defaults}
                setDefaults={setDefaults}
                options={options}
                setOptions={setOptions}
                proxyResources={proxyOptions}
                tagOptions={tagOptions}
                showSkipError={errorCount > 0}
                testModel={testModel}
              />
            </div>
          )}

          {step === 'progress' && (
            <ImportProgress
              items={queue.items}
              running={queue.running}
              runWindow={queue.runWindow}
              onRetry={(ids) => void queue.retry(ids)}
              onStop={queue.stop}
            />
          )}
        </div>

        <DialogFooter className="m-0 rounded-b-xl border-t p-4">
          {step === 'source' && (
            <Button onClick={() => parse(text)} disabled={!text.trim()}>
              识别
            </Button>
          )}
          {step === 'settings' && (
            <>
              <Button variant="ghost" onClick={() => setStep('source')}>
                上一步
              </Button>
              <Button onClick={start}>开始导入 {entries.length} 个账号</Button>
            </>
          )}
          {step === 'progress' && !queue.running && (
            <>
              <Button variant="ghost" onClick={reset}>
                导入其他账号
              </Button>
              <Button onClick={() => close(false)}>{s.failed || s.cancelled ? '关闭' : '完成'}</Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
