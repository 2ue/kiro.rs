import * as React from 'react'
import { Edit3, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { formatCompact, formatDate, formatNumber, formatPricePerMillion } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useDeleteManualModel,
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
} from '@/hooks/use-usage'
import type { ModelCapabilityItem, ModelPricing } from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  EmptyState,
  ErrorState,
  LoadingState,
  Callout,
  useConfirm,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Spinner,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  type BadgeProps,
} from '@/components/ui'
import { ManualModelModal, formFromModel, type ManualModelForm } from './manual-model-modal'

function sourceTone(source?: string): NonNullable<BadgeProps['tone']> {
  if (!source) return 'neutral'
  if (source === 'manual') return 'warning'
  if (source.includes('kiro')) return 'success'
  if (source === 'litellm') return 'info'
  if (source.includes('seed') || source === 'built-in') return 'primary'
  return 'neutral'
}

function sourceLabel(source?: string): string {
  if (!source) return '-'
  if (source === 'manual') return '手动'
  if (source.includes('kiro')) return '服务同步'
  if (source === 'litellm') return '价格源'
  if (source.includes('seed')) return 'Seed'
  if (source === 'built-in') return '内置'
  return source
}

function pricingByModel(pricing?: {
  models: { model: string; pricing: ModelPricing; source?: string }[]
}) {
  const map = new Map<string, { pricing: ModelPricing; source?: string }>()
  for (const item of pricing?.models || []) {
    map.set(item.model, { pricing: item.pricing, source: item.source })
  }
  return map
}

export function PricingPage() {
  const [manualOpen, setManualOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<ManualModelForm | null>(null)
  const pricing = useModelPricing()
  const syncPricing = useSyncModelPricing()
  const capabilities = useModelCapabilities()
  const syncCapabilities = useSyncModelCapabilities()
  const deleteManual = useDeleteManualModel()
  const priceMap = React.useMemo(() => pricingByModel(pricing.data), [pricing.data])
  const confirm = useConfirm()

  const openAdd = () => {
    setEditing(null)
    setManualOpen(true)
  }

  const openEdit = (item: ModelCapabilityItem) => {
    setEditing(formFromModel(item, priceMap.get(item.model)?.pricing))
    setManualOpen(true)
  }

  const removeManual = async (model: string) => {
    const confirmed = await confirm({
      title: '删除手动模型',
      message: `确认删除手动模型 ${model}？`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!confirmed) return
    deleteManual.mutate(model, {
      onSuccess: (response) => toast.success(response.message),
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

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
    <PageContainer>
      <PageHeader title={pageMeta.pricing.title} subtitle={pageMeta.pricing.subtitle} />
      <ManualModelModal open={manualOpen} initial={editing} onClose={() => setManualOpen(false)} />

      {/* 能力统计 */}
      <StatGrid>
        <StatCard
          title="模型能力"
          value={
            <Badge tone={capabilities.data?.available ? 'success' : 'error'}>
              {capabilities.data?.available ? '可用' : '不可用'}
            </Badge>
          }
        />
        <StatCard title="能力来源" value={capabilities.data?.source || '-'} />
        <StatCard title="模型能力数" value={formatNumber(capabilities.data?.modelCount || 0)} />
        <StatCard title="能力同步" value={formatDate(capabilities.data?.lastSyncedAt)} />
      </StatGrid>

      {/* 模型能力目录 */}
      <SectionCard
        title="Kiro 模型能力目录"
        description="从 Kiro 同步可用模型、上下文窗口、输出上限和缓存能力；手动模型作为补充保留。"
        noPadding
        actions={
          <>
            <Button size="sm" onClick={openAdd}>
              <Plus className="size-4" />
              手动添加模型
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={syncCapability}
              disabled={syncCapabilities.isPending}
            >
              {syncCapabilities.isPending ? <Spinner size="sm" /> : <RefreshCw className="size-4" />}
              同步模型能力
            </Button>
          </>
        }
      >
        {capabilities.data?.lastError && (
          <div className="px-5 pt-4">
            <Callout tone="warning">{capabilities.data.lastError}</Callout>
          </div>
        )}
        {capabilities.isLoading ? (
          <LoadingState />
        ) : capabilities.error ? (
          <div className="p-5">
            <ErrorState message={extractErrorMessage(capabilities.error)} />
          </div>
        ) : !capabilities.data?.models.length ? (
          <div className="p-5">
            <EmptyState title="暂无模型能力数据" />
          </div>
        ) : (
          <div className="max-h-[32rem] overflow-auto">
            <Table className="min-w-[1040px]">
              <TableHeader>
                <TableRow>
                  <TableHead>模型</TableHead>
                  <TableHead>显示名</TableHead>
                  <TableHead>来源</TableHead>
                  <TableHead className="text-right">输入上限</TableHead>
                  <TableHead className="text-right">输出上限</TableHead>
                  <TableHead className="text-right">缓存</TableHead>
                  <TableHead>输入类型</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {capabilities.data.models.map((item) => {
                  const isManual = item.source === 'manual'
                  return (
                    <TableRow key={item.model}>
                      <TableCell className="font-medium">{item.model}</TableCell>
                      <TableCell>{item.displayName}</TableCell>
                      <TableCell>
                        <Badge tone={sourceTone(item.source)}>{sourceLabel(item.source)}</Badge>
                      </TableCell>
                      <TableCell className="text-right font-mono tabular-nums">
                        {formatCompact(item.maxInputTokens)}
                      </TableCell>
                      <TableCell className="text-right font-mono tabular-nums">
                        {formatCompact(item.maxOutputTokens)}
                      </TableCell>
                      <TableCell className="text-right">
                        {item.supportsPromptCaching === undefined
                          ? '-'
                          : item.supportsPromptCaching
                            ? '支持'
                            : '不支持'}
                      </TableCell>
                      <TableCell className="text-muted-foreground">
                        {item.supportedInputTypes.length ? item.supportedInputTypes.join(', ') : '-'}
                      </TableCell>
                      <TableCell className="text-right">
                        {isManual ? (
                          <div className="flex justify-end gap-1">
                            <Button
                              variant="ghost"
                              size="icon-xs"
                              onClick={() => openEdit(item)}
                              title="编辑"
                            >
                              <Edit3 className="size-3.5" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-xs"
                              disabled={deleteManual.isPending}
                              onClick={() => removeManual(item.model)}
                              title="删除"
                            >
                              <Trash2 className="size-3.5" />
                            </Button>
                          </div>
                        ) : (
                          '-'
                        )}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
        )}
      </SectionCard>

      {/* 价格统计 */}
      <StatGrid>
        <StatCard
          title="价格状态"
          value={
            <Badge tone={pricing.data?.available ? 'success' : 'error'}>
              {pricing.data?.available ? '可用' : '不可用'}
            </Badge>
          }
        />
        <StatCard title="来源" value={pricing.data?.source || '-'} />
        <StatCard title="模型数" value={formatNumber(pricing.data?.modelCount || 0)} />
        <StatCard title="最后同步" value={formatDate(pricing.data?.lastSyncedAt)} />
      </StatGrid>

      {/* 价格目录 */}
      <SectionCard
        title="关注模型价格"
        description={pricing.data?.sourceUrl || '加载中...'}
        noPadding
        actions={
          <Button variant="outline" size="sm" onClick={syncPrice} disabled={syncPricing.isPending}>
            {syncPricing.isPending ? <Spinner size="sm" /> : <RefreshCw className="size-4" />}
            同步模型价格
          </Button>
        }
      >
        {pricing.data?.lastError && (
          <div className="px-5 pt-4">
            <Callout tone="warning">{pricing.data.lastError}</Callout>
          </div>
        )}
        {pricing.isLoading ? (
          <LoadingState />
        ) : pricing.error ? (
          <div className="p-5">
            <ErrorState message={extractErrorMessage(pricing.error)} />
          </div>
        ) : !pricing.data?.models.length ? (
          <div className="p-5">
            <EmptyState title="暂无价格数据" />
          </div>
        ) : (
          <Table className="min-w-[960px]">
            <TableHeader>
              <TableRow>
                <TableHead>模型</TableHead>
                <TableHead>来源</TableHead>
                <TableHead className="text-right">输入</TableHead>
                <TableHead className="text-right">输出</TableHead>
                <TableHead className="text-right">缓存写入</TableHead>
                <TableHead className="text-right">缓存读取</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {pricing.data.models.map((item) => (
                <TableRow key={item.model}>
                  <TableCell className="font-medium">{item.model}</TableCell>
                  <TableCell>
                    <Badge tone={sourceTone(item.source)}>{sourceLabel(item.source)}</Badge>
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatPricePerMillion(item.pricing.inputCostPerToken)}
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatPricePerMillion(item.pricing.outputCostPerToken)}
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatPricePerMillion(item.pricing.cacheCreationInputTokenCost)}
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatPricePerMillion(item.pricing.cacheReadInputTokenCost)}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </SectionCard>
    </PageContainer>
  )
}
