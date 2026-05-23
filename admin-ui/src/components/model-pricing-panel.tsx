import { RefreshCw, AlertTriangle } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { useModelPricing, useSyncModelPricing } from '@/hooks/use-usage'
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

export function ModelPricingPanel() {
  const pricing = useModelPricing()
  const syncPricing = useSyncModelPricing()
  const data = pricing.data

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

  return (
    <div className="space-y-4">
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
          手动同步
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
                    <th className="px-3 py-2 font-medium text-right">Cache Create</th>
                    <th className="px-3 py-2 font-medium text-right">Cache Read</th>
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
