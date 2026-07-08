import { useEffect, useMemo, useState } from 'react'
import { CheckCircle2, Loader2, Play, RotateCw, XCircle } from 'lucide-react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { useTestCredential } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS, testModelLabel } from '@/lib/test-models'
import type { CredentialStatusItem, TestCredentialResponse } from '@/types/api'

interface CredentialTestDialogProps {
  credential: CredentialStatusItem | null
  open: boolean
  onOpenChange: (open: boolean) => void
}

function credentialName(credential: CredentialStatusItem) {
  return credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
}

function authLabel(authMethod: string | null) {
  if (authMethod === 'api_key') return 'APIKEY'
  if (authMethod === 'idc') return 'IDC'
  if (authMethod === 'social') return 'SOCIAL'
  return authMethod?.toUpperCase() || 'UNKNOWN'
}

export function CredentialTestDialog({
  credential,
  open,
  onOpenChange,
}: CredentialTestDialogProps) {
  const [model, setModel] = useState(DEFAULT_TEST_MODEL)
  const [prompt, setPrompt] = useState(DEFAULT_TEST_PROMPT)
  const [result, setResult] = useState<TestCredentialResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const testCredential = useTestCredential()

  const selectedModelLabel = useMemo(
    () => testModelLabel(model),
    [model]
  )

  useEffect(() => {
    if (!open) {
      return
    }
    setResult(null)
    setError(null)
    setPrompt(DEFAULT_TEST_PROMPT)
  }, [open, credential?.id])

  const handleRun = () => {
    if (!credential) {
      return
    }
    const trimmedPrompt = prompt.trim()
    if (!trimmedPrompt) {
      toast.error('测试消息不能为空')
      return
    }

    setResult(null)
    setError(null)
    testCredential.mutate(
      {
        id: credential.id,
        request: {
          model,
          prompt: trimmedPrompt,
        },
      },
      {
        onSuccess: (response) => {
          setResult(response)
          toast.success(`凭据 #${response.credentialId} 测试完成`)
        },
        onError: (err) => {
          setError(extractErrorMessage(err))
        },
      }
    )
  }

  const isRunning = testCredential.isPending
  const canRun = Boolean(credential) && !isRunning

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>测试模型调用</DialogTitle>
        </DialogHeader>

        {credential && (
          <div className="space-y-4">
            <div className="flex items-center justify-between gap-3 rounded-lg border bg-muted/30 p-4">
              <div className="flex min-w-0 items-center gap-3">
                <div className="flex h-12 w-12 shrink-0 items-center justify-center rounded-lg bg-teal-600 text-white">
                  <Play className="h-6 w-6" />
                </div>
                <div className="min-w-0">
                  <div className="truncate text-lg font-semibold">
                    {credentialName(credential)}
                  </div>
                  <div className="mt-1 flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
                    <Badge variant="secondary">{authLabel(credential.authMethod)}</Badge>
                    <span>账号 #{credential.id}</span>
                    {credential.endpoint && <span>endpoint: {credential.endpoint}</span>}
                  </div>
                </div>
              </div>
              <Badge variant={credential.disabled ? 'destructive' : 'success'}>
                {credential.disabled ? 'disabled' : 'active'}
              </Badge>
            </div>

            <div className="grid gap-3 sm:grid-cols-[1fr_180px]">
              <label className="space-y-2">
                <span className="text-sm font-medium">选择测试模型</span>
                <select
                  value={model}
                  onChange={(event) => setModel(event.target.value)}
                  disabled={isRunning}
                  className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {TEST_MODELS.map((option) => (
                    <option key={option.id} value={option.id}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="space-y-2">
                <span className="text-sm font-medium">测试消息</span>
                <Input
                  value={prompt}
                  onChange={(event) => setPrompt(event.target.value)}
                  disabled={isRunning}
                />
              </label>
            </div>

            <div className="rounded-lg border bg-slate-950 p-4 font-mono text-sm text-slate-200">
              <div className="space-y-1">
                <div>
                  <span className="text-blue-400">凭据：</span>
                  <span className="text-blue-300"> {credentialName(credential)}</span>
                </div>
                <div>
                  <span className="text-cyan-300">使用模型：</span>
                  <span className="text-cyan-200"> {model}</span>
                </div>
                <div>
                  <span className="text-slate-400">发送测试消息：</span>
                  <span className="text-slate-300"> "{prompt.trim() || 'hi'}"</span>
                </div>
              </div>

              <div className="mt-4 border-t border-slate-700 pt-4">
                {isRunning && (
                  <div className="flex items-center gap-2 text-blue-300">
                    <Loader2 className="h-4 w-4 animate-spin" />
                    正在等待模型响应...
                  </div>
                )}
                {result && (
                  <div className="space-y-3">
                    <div>
                      <div className="mb-1 text-yellow-300">响应：</div>
                      <div className="whitespace-pre-wrap break-words text-emerald-200">
                        {result.response}
                      </div>
                    </div>
                    <div className="border-t border-slate-700 pt-3 text-emerald-400">
                      <CheckCircle2 className="mr-2 inline h-4 w-4" />
                      测试完成！耗时 {result.durationMs}ms
                    </div>
                  </div>
                )}
                {error && (
                  <div className="space-y-2 text-red-300">
                    <div>
                      <XCircle className="mr-2 inline h-4 w-4" />
                      测试失败
                    </div>
                    <div className="whitespace-pre-wrap break-words text-red-200">{error}</div>
                  </div>
                )}
                {!isRunning && !result && !error && (
                  <div className="text-slate-400">等待开始测试</div>
                )}
              </div>
            </div>

            <div className="flex flex-wrap justify-between gap-3 text-sm text-muted-foreground">
              <span>测试模型：{selectedModelLabel}</span>
              <span>提示词："{prompt.trim() || 'hi'}"</span>
            </div>
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={isRunning}>
            关闭
          </Button>
          <Button onClick={handleRun} disabled={!canRun}>
            {isRunning ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : result || error ? (
              <RotateCw className="mr-2 h-4 w-4" />
            ) : (
              <Play className="mr-2 h-4 w-4" />
            )}
            {result || error ? '重试' : '开始测试'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
