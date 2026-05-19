import { useEffect, useMemo, useState } from 'react'
import { CheckCircle2, Loader2, PlayCircle, XCircle } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import type {
  CredentialStatusItem,
  CredentialTestResponse,
  ModelPrice,
} from '@/types/api'
import { useCredentialTest, usePricing } from '@/hooks/use-credentials'

interface CredentialTestDialogProps {
  credential: CredentialStatusItem
  open: boolean
  onOpenChange: (open: boolean) => void
}

function displayName(credential: CredentialStatusItem): string {
  return credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
}

function pickDefaultModel(models: ModelPrice[]): string {
  return models[0]?.modelId || ''
}

function responsePreview(result: CredentialTestResponse): string {
  if (result.success) {
    return result.outputText || '测试成功，但没有返回文本内容。'
  }
  return result.errorMessage || result.rawPreview || result.errorType || '测试失败'
}

function errorPreview(error: unknown): string {
  const maybeError = error as {
    response?: {
      data?: {
        error?: { message?: string }
        errorMessage?: string
        message?: string
      }
      status?: number
      statusText?: string
    }
    message?: string
  }
  if (maybeError?.response?.data !== undefined) {
    return (
      maybeError.response.data.error?.message ||
      maybeError.response.data.errorMessage ||
      maybeError.response.data.message ||
      maybeError.response.statusText ||
      `HTTP ${maybeError.response.status ?? ''}`.trim()
    )
  }
  if (maybeError?.response) {
    return maybeError.response.statusText || `HTTP ${maybeError.response.status ?? ''}`.trim()
  }
  return maybeError?.message || '测试失败'
}

export function CredentialTestDialog({
  credential,
  open,
  onOpenChange,
}: CredentialTestDialogProps) {
  const pricing = usePricing(open)
  const testCredential = useCredentialTest()
  const [model, setModel] = useState('')
  const [preview, setPreview] = useState('等待开始测试')
  const [previewStatus, setPreviewStatus] = useState<'idle' | 'success' | 'error'>('idle')

  const availableModels = useMemo(() => pricing.data ?? [], [pricing.data])

  useEffect(() => {
    if (!open) return
    setPreview('等待开始测试')
    setPreviewStatus('idle')
  }, [open])

  useEffect(() => {
    if (!open || model || availableModels.length === 0) return
    setModel(pickDefaultModel(availableModels))
  }, [availableModels, model, open])

  const handleStart = () => {
    if (!model) {
      setPreview('请选择模型')
      setPreviewStatus('error')
      return
    }

    setPreview('测试中...')
    setPreviewStatus('idle')
    testCredential.mutate(
      { id: credential.id, request: { model } },
      {
        onSuccess: (response) => {
          setPreview(responsePreview(response))
          setPreviewStatus(response.success ? 'success' : 'error')
        },
        onError: (error) => {
          setPreview(errorPreview(error))
          setPreviewStatus('error')
        },
      },
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>账号测试</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <div className="rounded-md border bg-muted/30 p-3 text-sm">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-medium">{displayName(credential)}</span>
              <Badge variant="outline">#{credential.id}</Badge>
              {credential.endpoint && <Badge variant="secondary">{credential.endpoint}</Badge>}
              {credential.disabled && <Badge variant="destructive">已禁用</Badge>}
            </div>
            <div className="mt-2 text-xs text-muted-foreground">
              成功 {credential.successCount.toLocaleString('zh-CN')} · 失败 {credential.failureCount}
              {credential.refreshFailureCount > 0 && ` · 刷新失败 ${credential.refreshFailureCount}`}
            </div>
          </div>

          <div className="space-y-2">
            <label className="text-sm font-medium" htmlFor={`credential-test-model-${credential.id}`}>
              模型
            </label>
            <select
              id={`credential-test-model-${credential.id}`}
              className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
              value={model}
              onChange={(event) => setModel(event.target.value)}
              disabled={pricing.isLoading || testCredential.isPending}
            >
              {availableModels.length === 0 ? (
                <option value="">加载模型中...</option>
              ) : (
                availableModels.map((item) => (
                  <option key={item.modelId} value={item.modelId}>
                    {item.modelId}
                  </option>
                ))
              )}
            </select>
          </div>

          <div className="rounded-md border bg-zinc-950 p-3 text-xs text-zinc-100">
            <div className="mb-2 flex items-center gap-2 text-zinc-300">
              {testCredential.isPending ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : previewStatus === 'success' ? (
                <CheckCircle2 className="h-4 w-4 text-emerald-400" />
              ) : previewStatus === 'error' ? (
                <XCircle className="h-4 w-4 text-red-400" />
              ) : (
                <PlayCircle className="h-4 w-4" />
              )}
              <span>预览</span>
            </div>
            <pre className="max-h-64 min-h-40 overflow-auto whitespace-pre-wrap break-words font-mono">
              {testCredential.isPending ? '测试中...' : preview}
            </pre>
          </div>
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={testCredential.isPending}
          >
            关闭
          </Button>
          <Button
            onClick={handleStart}
            disabled={testCredential.isPending || pricing.isLoading || !model}
          >
            {testCredential.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <PlayCircle className="h-4 w-4" />
            )}
            开始测试
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
