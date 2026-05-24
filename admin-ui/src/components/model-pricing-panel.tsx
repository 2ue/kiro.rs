import { RefreshCw, AlertTriangle } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
} from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'

function formatDate(value?: string): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function formatPrice(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return `$${(value * 1_000_000).toFixed(2)}/M`
}

function formatTokens(value?: number): string {
  if (!value || !Number.isFinite(value)) return '-'
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`
  if (value >= 1000) return `${Math.round(value / 1000)}K`
  return String(value)
}

export function ModelPricingPanel() {
  const pricing = useModelPricing()
  const syncPricing = useSyncModelPricing()
  const capabilities = useModelCapabilities()
  const syncCapabilities = useSyncModelCapabilities()
  const data = pricing.data
  const capabilityData = capabilities.data

  const handleSync = () => {
    syncPricing.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) {
          toast.warning(`同步失败，继续使用当前价格目录: ${status.lastError}`)
        } else {
          toast.success(`模型价格已同步：${status.modelCount} 个模型`)
        }
      },
      onError: (error) => {
        toast.error(`同步失败: ${extractErrorMessage(error)}`)
      },
    })
  }

  const handleSyncCapabilities = () => {
    syncCapabilities.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) {
          toast.warning(`模型能力同步失败，继续使用当前目录: ${status.lastError}`)
        } else {
          toast.success(`模型能力已同步：${status.modelCount} 个模型`)
        }
      },
      onError: (error) => {
        toast.error(`同步失败: ${extractErrorMessage(error)}`)
      },
    })
  }

  return (
    <div className="space-y-4">
      <div className="grid gap-4 md:grid-cols-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">模型能力</CardTitle>
          </CardHeader>
          <CardContent>
            <Badge variant={capabilityData?.available ? 'success' : 'destructive'}>
              {capabilityData?.available ? '可用' : '不可用'}
            </Badge>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">能力来源</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-xl font-bold">{capabilityData?.source || '-'}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">模型能力数</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-xl font-bold">{capabilityData?.modelCount || 0}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">能力同步</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-sm font-medium">{formatDate(capabilityData?.lastSyncedAt)}</div>
          </CardContent>
        </Card>
      </div>

      <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 md:flex-row md:items-center md:justify-between">
        <div className="min-w-0">
          <div className="font-medium">Kiro 模型能力目录</div>
          <div className="text-sm text-muted-foreground">
            从 Kiro 上游同步可用模型、上下文窗口、输出上限和缓存能力；失败不影响请求调度。
          </div>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={handleSyncCapabilities}
          disabled={syncCapabilities.isPending}
        >
          <RefreshCw className={`h-4 w-4 ${syncCapabilities.isPending ? 'animate-spin' : ''}`} />
          同步模型能力
        </Button>
      </div>

      {capabilityData?.lastError && (
        <div className="flex items-start gap-2 rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <div className="break-all">{capabilityData.lastError}</div>
        </div>
      )}

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">模型能力</CardTitle>
        </CardHeader>
        <CardContent>
          {capabilities.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : capabilities.error ? (
            <div className="py-8 text-center text-destructive">{extractErrorMessage(capabilities.error)}</div>
          ) : !capabilityData?.models.length ? (
            <div className="py-8 text-center text-muted-foreground">暂无模型能力数据</div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[900px] text-sm">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">模型</th>
                    <th className="px-3 py-2 font-medium">显示名</th>
                    <th className="px-3 py-2 font-medium text-right">输入上限</th>
                    <th className="px-3 py-2 font-medium text-right">输出上限</th>
                    <th className="px-3 py-2 font-medium text-right">缓存</th>
                    <th className="px-3 py-2 font-medium">输入类型</th>
                  </tr>
                </thead>
                <tbody>
                  {capabilityData.models.map((item) => (
                    <tr key={item.model} className="border-b last:border-0">
                      <td className="px-3 py-2 font-medium">{item.model}</td>
                      <td className="px-3 py-2">{item.displayName}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatTokens(item.maxInputTokens)}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatTokens(item.maxOutputTokens)}</td>
                      <td className="px-3 py-2 text-right">
                        {item.supportsPromptCaching === undefined ? '-' : item.supportsPromptCaching ? '支持' : '不支持'}
                      </td>
                      <td className="px-3 py-2 text-muted-foreground">
                        {item.supportedInputTypes.length ? item.supportedInputTypes.join(', ') : '-'}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>

      <div className="grid gap-4 md:grid-cols-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">状态</CardTitle>
          </CardHeader>
          <CardContent>
            <Badge variant={data?.available ? 'success' : 'destructive'}>
              {data?.available ? '可用' : '不可用'}
            </Badge>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">来源</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-xl font-bold">{data?.source || '-'}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">模型数</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-xl font-bold">{data?.modelCount || 0}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">最后同步</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-sm font-medium">{formatDate(data?.lastSyncedAt)}</div>
          </CardContent>
        </Card>
      </div>

      <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 md:flex-row md:items-center md:justify-between">
        <div className="min-w-0">
          <div className="font-medium">模型价格目录</div>
          <div className="truncate text-sm text-muted-foreground" title={data?.sourceUrl}>
            {data?.sourceUrl || '加载中...'}
          </div>
        </div>
        <Button variant="outline" size="sm" onClick={handleSync} disabled={syncPricing.isPending}>
          <RefreshCw className={`h-4 w-4 ${syncPricing.isPending ? 'animate-spin' : ''}`} />
          同步模型价格
        </Button>
      </div>

      {data?.lastError && (
        <div className="flex items-start gap-2 rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <div className="break-all">{data.lastError}</div>
        </div>
      )}

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">关注模型价格</CardTitle>
        </CardHeader>
        <CardContent>
          {pricing.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : pricing.error ? (
            <div className="py-8 text-center text-destructive">{extractErrorMessage(pricing.error)}</div>
          ) : !data?.models.length ? (
            <div className="py-8 text-center text-muted-foreground">暂无价格数据</div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[900px] text-sm">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">模型</th>
                    <th className="px-3 py-2 font-medium text-right">输入</th>
                    <th className="px-3 py-2 font-medium text-right">输出</th>
                    <th className="px-3 py-2 font-medium text-right">缓存写入</th>
                    <th className="px-3 py-2 font-medium text-right">缓存读取</th>
                  </tr>
                </thead>
                <tbody>
                  {data.models.map((item) => (
                    <tr key={item.model} className="border-b last:border-0">
                      <td className="px-3 py-2 font-medium">{item.model}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.inputCostPerToken)}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.outputCostPerToken)}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.cacheCreationInputTokenCost)}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.cacheReadInputTokenCost)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
