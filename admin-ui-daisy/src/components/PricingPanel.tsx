import { AlertTriangle, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'
import { Alert as DaisyAlert, Button, Loading, Table } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, LoadingState, SectionCard, StatCard } from '@/components/common'
import { formatCompact, formatDate, formatNumber, formatPricePerMillion } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
} from '@/hooks/use-usage'

export function PricingPanel() {
  const pricing = useModelPricing()
  const syncPricing = useSyncModelPricing()
  const capabilities = useModelCapabilities()
  const syncCapabilities = useSyncModelCapabilities()

  const syncPrice = () => {
    syncPricing.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) toast.warning(`同步失败，继续使用当前价格目录: ${status.lastError}`)
        else toast.success(`模型价格已同步：${status.modelCount} 个模型`)
      },
      onError: (error) => toast.error(`同步失败: ${extractErrorMessage(error)}`),
    })
  }

  const syncCapability = () => {
    syncCapabilities.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) toast.warning(`模型能力同步失败，继续使用当前目录: ${status.lastError}`)
        else toast.success(`模型能力已同步：${status.modelCount} 个模型`)
      },
      onError: (error) => toast.error(`同步失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <div className="space-y-4">
      <div className="metric-grid">
        <StatCard title="模型能力" value={<Badge tone={capabilities.data?.available ? 'success' : 'error'}>{capabilities.data?.available ? '可用' : '不可用'}</Badge>} />
        <StatCard title="能力来源" value={capabilities.data?.source || '-'} />
        <StatCard title="模型能力数" value={formatNumber(capabilities.data?.modelCount || 0)} />
        <StatCard title="能力同步" value={formatDate(capabilities.data?.lastSyncedAt)} />
      </div>

      <SectionCard
        title="Kiro 模型能力目录"
        description="从 Kiro 上游同步可用模型、上下文窗口、输出上限和缓存能力；失败不影响请求调度。"
        actions={
          <Button type="button" variant="outline" size="sm" onClick={syncCapability} disabled={syncCapabilities.isPending}>
            {syncCapabilities.isPending ? <Loading size="xs" /> : <RefreshCw className="h-4 w-4" />}
            同步模型能力
          </Button>
        }
      >
        {capabilities.data?.lastError && <WarningAlert text={capabilities.data.lastError} />}
        {capabilities.isLoading ? (
          <LoadingState />
        ) : capabilities.error ? (
          <ErrorState text={extractErrorMessage(capabilities.error)} />
        ) : !capabilities.data?.models.length ? (
          <EmptyState text="暂无模型能力数据" />
        ) : (
          <div className="table-panel table-panel-tall">
            <Table zebra size="sm" className="data-table min-w-[900px]">
              <Table.Head>
                <span>模型</span>
                <span>显示名</span>
                <span className="text-right">输入上限</span>
                <span className="text-right">输出上限</span>
                <span className="text-right">缓存</span>
                <span>输入类型</span>
              </Table.Head>
              <Table.Body>
                {capabilities.data.models.map((item) => (
                  <Table.Row key={item.model} hover>
                    <span className="font-medium">{item.model}</span>
                    <span>{item.displayName}</span>
                    <span className="text-right font-mono">{formatCompact(item.maxInputTokens)}</span>
                    <span className="text-right font-mono">{formatCompact(item.maxOutputTokens)}</span>
                    <span className="text-right">{item.supportsPromptCaching === undefined ? '-' : item.supportsPromptCaching ? '支持' : '不支持'}</span>
                    <span className="text-base-content/60">{item.supportedInputTypes.length ? item.supportedInputTypes.join(', ') : '-'}</span>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          </div>
        )}
      </SectionCard>

      <div className="metric-grid">
        <StatCard title="价格状态" value={<Badge tone={pricing.data?.available ? 'success' : 'error'}>{pricing.data?.available ? '可用' : '不可用'}</Badge>} />
        <StatCard title="来源" value={pricing.data?.source || '-'} />
        <StatCard title="模型数" value={formatNumber(pricing.data?.modelCount || 0)} />
        <StatCard title="最后同步" value={formatDate(pricing.data?.lastSyncedAt)} />
      </div>

      <SectionCard
        title="关注模型价格"
        description={pricing.data?.sourceUrl || '加载中...'}
        actions={
          <Button type="button" variant="outline" size="sm" onClick={syncPrice} disabled={syncPricing.isPending}>
            {syncPricing.isPending ? <Loading size="xs" /> : <RefreshCw className="h-4 w-4" />}
            同步模型价格
          </Button>
        }
      >
        {pricing.data?.lastError && <WarningAlert text={pricing.data.lastError} />}
        {pricing.isLoading ? (
          <LoadingState />
        ) : pricing.error ? (
          <ErrorState text={extractErrorMessage(pricing.error)} />
        ) : !pricing.data?.models.length ? (
          <EmptyState text="暂无价格数据" />
        ) : (
          <div className="table-panel">
            <Table zebra size="sm" className="data-table min-w-[900px]">
              <Table.Head>
                <span>模型</span>
                <span className="text-right">输入</span>
                <span className="text-right">输出</span>
                <span className="text-right">缓存写入</span>
                <span className="text-right">缓存读取</span>
              </Table.Head>
              <Table.Body>
                {pricing.data.models.map((item) => (
                  <Table.Row key={item.model} hover>
                    <span className="font-medium">{item.model}</span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.inputCostPerToken)}</span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.outputCostPerToken)}</span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.cacheCreationInputTokenCost)}</span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.cacheReadInputTokenCost)}</span>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          </div>
        )}
      </SectionCard>
    </div>
  )
}

function WarningAlert({ text }: { text: string }) {
  return (
    <DaisyAlert status="warning" className="mb-4 text-sm">
      <AlertTriangle className="h-4 w-4" />
      <span className="break-all">{text}</span>
    </DaisyAlert>
  )
}
