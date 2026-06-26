import { useEffect, useMemo, useState } from 'react'
import { CheckCircle2, Loader2, Play, RotateCw, XCircle } from 'lucide-react'
import { toast } from 'sonner'
import { testExternalPool } from '@/api/credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS } from '@/lib/test-models'
import type { ExternalPool, ExternalPoolTestResponse } from '@/types/api'
import { ModalShell } from '@/components/patterns'
import { Badge, Button, Input, Label } from '@/components/ui'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui'

export function ExternalPoolTestModal({
  pool,
  open,
  onClose,
  onDone,
}: {
  pool: ExternalPool | null
  open: boolean
  onClose: () => void
  onDone: () => void
}) {
  const modelCapabilities = useModelCapabilities()
  const [model, setModel] = useState(DEFAULT_TEST_MODEL)
  const [prompt, setPrompt] = useState(DEFAULT_TEST_PROMPT)
  const [result, setResult] = useState<ExternalPoolTestResponse | null>(null)
  const [error, setError] = useState('')
  const [running, setRunning] = useState(false)

  const modelOptions = useMemo(() => {
    const seen = new Set<string>()
    const options: { id: string; label: string }[] = []
    const push = (id: string, label: string) => {
      const key = id.trim()
      if (!key || seen.has(key)) return
      seen.add(key)
      options.push({ id: key, label })
    }
    TEST_MODELS.forEach((item) => push(item.id, item.label))
    ;[...(modelCapabilities.data?.models || [])]
      .sort((a, b) => a.model.localeCompare(b.model))
      .forEach((item) => push(item.model, item.displayName || item.model))
    return options
  }, [modelCapabilities.data?.models])

  const selectedModelLabel = useMemo(
    () => modelOptions.find((o) => o.id === model)?.label || model,
    [model, modelOptions]
  )

  useEffect(() => {
    if (!open) return
    setModel(DEFAULT_TEST_MODEL)
    setPrompt(DEFAULT_TEST_PROMPT)
    setResult(null)
    setError('')
    setRunning(false)
  }, [open, pool?.id])

  const run = async () => {
    if (!pool) return
    const trimmedModel = model.trim()
    const trimmedPrompt = prompt.trim() || DEFAULT_TEST_PROMPT
    if (!trimmedModel) { toast.error('请选择或输入测试模型'); return }
    setRunning(true); setResult(null); setError('')
    try {
      const response = await testExternalPool(pool.id, { model: trimmedModel, prompt: trimmedPrompt })
      setResult(response)
      if (response.ok) toast.success(response.message || '外部账号模型调用测试通过')
      else toast.error(response.message || '外部账号模型调用测试失败')
      onDone()
    } catch (err) {
      setError(extractErrorMessage(err))
    } finally {
      setRunning(false)
    }
  }

  return (
    <ModalShell
      open={open}
      title="测试外部账号"
      width="max-w-2xl"
      onClose={onClose}
      footer={
        <>
          <Button type="button" variant="ghost" size="sm" onClick={onClose} disabled={running}>关闭</Button>
          <Button type="button" size="sm" onClick={run} disabled={!pool || running}>
            {running ? <Loader2 className="h-4 w-4 animate-spin" /> : result || error ? <RotateCw className="h-4 w-4" /> : <Play className="h-4 w-4" />}
            {result || error ? '重试' : '开始测试'}
          </Button>
        </>
      }
    >
      {pool && (
        <div className="space-y-4">
          <div className="rounded-lg border border-border bg-surface-subtle p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-semibold">#{pool.id} {pool.name}</span>
              <Badge tone="neutral">{pool.authType}</Badge>
              <Badge tone={pool.enabled ? 'success' : 'error'}>{pool.enabled ? '启用' : '已禁用'}</Badge>
            </div>
            <div className="mt-1 break-all text-xs text-muted-foreground">{pool.baseUrl}</div>
          </div>

          <div className="grid gap-3 sm:grid-cols-[1fr_200px]">
            <div className="space-y-1">
              <Label>测试模型</Label>
              <Select value={model} onValueChange={setModel} disabled={running}>
                <SelectTrigger size="sm"><SelectValue placeholder="选择测试模型" /></SelectTrigger>
                <SelectContent>
                  {modelOptions.map((option) => (
                    <SelectItem key={option.id} value={option.id}>{option.label}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1">
              <Label>测试消息</Label>
              <Input size={undefined} className="h-9 text-sm" value={prompt} disabled={running} onChange={(e) => setPrompt(e.target.value)} />
            </div>
          </div>

          <div className="rounded-lg border border-border bg-surface-subtle p-4 font-mono text-xs">
            <div className="space-y-1 text-muted-foreground">
              <div><span className="text-info">外部账号：</span> #{pool.id} {pool.name}</div>
              <div><span className="text-info">使用模型：</span> {model}</div>
              <div><span className="text-muted-foreground">发送消息：</span> "{prompt.trim() || DEFAULT_TEST_PROMPT}"</div>
            </div>
            <div className="mt-3 border-t border-border pt-3">
              {running && (
                <div className="flex items-center gap-2 text-info">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  正在等待外部账号模型响应...
                </div>
              )}
              {result && (
                <div className={result.ok ? 'space-y-2 text-success' : 'space-y-2 text-destructive'}>
                  <div>
                    {result.ok ? <CheckCircle2 className="mr-2 inline h-4 w-4" /> : <XCircle className="mr-2 inline h-4 w-4" />}
                    {result.message}
                  </div>
                  <div className="text-muted-foreground">HTTP 状态：{result.status ?? '-'}</div>
                  {result.model && <div className="text-muted-foreground">返回模型：{result.model}</div>}
                  {result.response && (
                    <div>
                      <div className="mb-1 text-warning">响应：</div>
                      <div className="whitespace-pre-wrap break-words">{result.response}</div>
                    </div>
                  )}
                </div>
              )}
              {error && (
                <div className="space-y-2 text-destructive">
                  <div><XCircle className="mr-2 inline h-4 w-4" />测试失败</div>
                  <div className="whitespace-pre-wrap break-words">{error}</div>
                </div>
              )}
              {!running && !result && !error && <div className="text-muted-foreground">等待开始测试</div>}
            </div>
          </div>

          <div className="flex flex-wrap justify-between gap-3 text-xs text-muted-foreground">
            <span>测试模型：{selectedModelLabel}</span>
            <span>提示词："{prompt.trim() || DEFAULT_TEST_PROMPT}"</span>
          </div>
        </div>
      )}
    </ModalShell>
  )
}
