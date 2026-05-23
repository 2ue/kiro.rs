import { useEffect, useState } from 'react'
import { Save } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { RuntimeConfig } from '@/types/api'

const emptyConfig: RuntimeConfig = {
  credentialRpm: 0,
  credentialTransientCooldownSecs: 10,
  credentialMaxCooldownSecs: 300,
  credentialWarmupRequests: 3,
  credentialWarmupSelectionPercent: 5,
  compressionEnabled: false,
  whitespaceCompression: true,
}

function toNumber(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

export function RuntimeConfigPanel() {
  const config = useRuntimeConfig()
  const updateConfig = useUpdateRuntimeConfig()
  const [draft, setDraft] = useState<RuntimeConfig>(emptyConfig)

  useEffect(() => {
    if (config.data) {
      setDraft(config.data)
    }
  }, [config.data])

  const handleSave = () => {
    const next: RuntimeConfig = {
      ...draft,
      credentialRpm: Math.max(0, Math.floor(draft.credentialRpm || 0)),
      credentialTransientCooldownSecs: Math.max(
        1,
        Math.floor(draft.credentialTransientCooldownSecs || 1)
      ),
      credentialMaxCooldownSecs: Math.max(
        1,
        Math.floor(draft.credentialMaxCooldownSecs || 1)
      ),
      credentialWarmupRequests: Math.max(0, Math.floor(draft.credentialWarmupRequests || 0)),
      credentialWarmupSelectionPercent: Math.min(
        100,
        Math.max(0, Math.floor(draft.credentialWarmupSelectionPercent || 0))
      ),
    }
    if (next.credentialTransientCooldownSecs > next.credentialMaxCooldownSecs) {
      toast.error('Transient Cooldown 不能大于 Max Cooldown')
      return
    }
    updateConfig.mutate(next, {
      onSuccess: () => toast.success('配置已更新'),
      onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
    })
  }

  if (config.isLoading) {
    return <div className="py-8 text-center text-muted-foreground">加载中...</div>
  }

  if (config.error) {
    return (
      <Card>
        <CardContent className="py-8 text-center text-destructive">
          {extractErrorMessage(config.error)}
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle className="text-base">运行时配置</CardTitle>
        </CardHeader>
        <CardContent className="space-y-6">
          <div className="grid gap-4 md:grid-cols-2">
            <label className="space-y-2">
              <span className="text-sm font-medium">Credential RPM</span>
              <Input
                value={draft.credentialRpm}
                inputMode="numeric"
                onChange={(event) =>
                  setDraft((prev) => ({
                    ...prev,
                    credentialRpm: toNumber(event.target.value, 0),
                  }))
                }
              />
              <span className="block text-xs text-muted-foreground">0 表示关闭本地凭据级限速</span>
            </label>
            <label className="space-y-2">
              <span className="text-sm font-medium">Transient Cooldown Seconds</span>
              <Input
                value={draft.credentialTransientCooldownSecs}
                inputMode="numeric"
                onChange={(event) =>
                  setDraft((prev) => ({
                    ...prev,
                    credentialTransientCooldownSecs: toNumber(event.target.value, 1),
                  }))
                }
              />
              <span className="block text-xs text-muted-foreground">无 Retry-After 时使用</span>
            </label>
            <label className="space-y-2">
              <span className="text-sm font-medium">Max Cooldown Seconds</span>
              <Input
                value={draft.credentialMaxCooldownSecs}
                inputMode="numeric"
                onChange={(event) =>
                  setDraft((prev) => ({
                    ...prev,
                    credentialMaxCooldownSecs: toNumber(event.target.value, 1),
                  }))
                }
              />
            </label>
            <label className="space-y-2">
              <span className="text-sm font-medium">Warmup Requests</span>
              <Input
                value={draft.credentialWarmupRequests}
                inputMode="numeric"
                onChange={(event) =>
                  setDraft((prev) => ({
                    ...prev,
                    credentialWarmupRequests: toNumber(event.target.value, 0),
                  }))
                }
              />
            </label>
            <label className="space-y-2">
              <span className="text-sm font-medium">Warmup Selection Percent</span>
              <Input
                value={draft.credentialWarmupSelectionPercent}
                inputMode="numeric"
                onChange={(event) =>
                  setDraft((prev) => ({
                    ...prev,
                    credentialWarmupSelectionPercent: toNumber(event.target.value, 0),
                  }))
                }
              />
              <span className="block text-xs text-muted-foreground">
                balanced 模式下预热凭据参与真实请求的概率
              </span>
            </label>
          </div>

          <div className="grid gap-4 md:grid-cols-2">
            <label className="flex items-center justify-between rounded-md border p-3">
              <span className="text-sm font-medium">Compression</span>
              <Switch
                checked={draft.compressionEnabled}
                onCheckedChange={(checked) =>
                  setDraft((prev) => ({ ...prev, compressionEnabled: checked }))
                }
              />
            </label>
            <label className="flex items-center justify-between rounded-md border p-3">
              <span className="text-sm font-medium">Whitespace Compression</span>
              <Switch
                checked={draft.whitespaceCompression}
                onCheckedChange={(checked) =>
                  setDraft((prev) => ({ ...prev, whitespaceCompression: checked }))
                }
              />
            </label>
          </div>

          <div className="flex justify-end">
            <Button onClick={handleSave} disabled={updateConfig.isPending}>
              <Save className="h-4 w-4" />
              {updateConfig.isPending ? '保存中...' : '保存'}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
