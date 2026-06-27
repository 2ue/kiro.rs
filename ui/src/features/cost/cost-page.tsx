import { useMemo, useState } from 'react'
import {
  DollarSign,
  Edit3,
  Plus,
  RefreshCw,
  Trash2,
  TrendingDown,
  TrendingUp,
} from 'lucide-react'
import { toast } from 'sonner'
import { formatCompact, formatDate, formatNumber, formatPricePerMillion, formatUsd } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useDeleteManualModel,
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
  useUsageSummary,
} from '@/hooks/use-usage'
import type { ModelCapabilityItem, ModelPriceItem } from '@/types/api'
import type { BadgeProps } from '@/components/ui'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  EmptyState,
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
} from '@/components/ui'
import { ManualModelModal, formFromCapability, type ManualModelForm } from './manual-model-modal'

// ─── 帮助函数 ──────────────────────────────────────────────────────────────────

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

function pricingByModel(pricing?: { models: ModelPriceItem[] }) {
  const map = new Map<string, ModelPriceItem>()
  for (const item of pricing?.models ?? []) map.set(item.model, item)
  return map
}

// ─── 模型能力目录 ──────────────────────────────────────────────────────────────

function ModelCapabilitiesTable({
  onAdd,
  onEdit,
  onDelete,
}: {
  onAdd: () => void
  onEdit: (form: ManualModelForm) => void
  onDelete: (model: string) => void
}) {
  const capabilities = useModelCapabilities()
  const pricing = useModelPricing()
  const syncCapabilities = useSyncModelCapabilities()
  const priceMap = useMemo(() => {
    const map = new Map<string, ModelPriceItem>()
    for (const item of pricing.data?.models ?? []) map.set(item.model, item)
    return map
  }, [pricing.data?.models])

  return (
    <SectionCard
      title="模型能力目录"
      description="从 Kiro 同步的可用模型、上下文窗口、输出上限和缓存能力；手动模型作为补充。"
      actions={
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" onClick={onAdd}>
            <Plus className="h-3.5 w-3.5" />添加手动模型
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              syncCapabilities.mutate(undefined, {
                onSuccess: (s) => {
                  if (s.lastError) toast.warning(`同步失败: ${s.lastError}`)
                  else toast.success(`模型能力已同步：${s.modelCount} 个模型`)
                },
                onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
              })
            }}
            disabled={syncCapabilities.isPending}
          >
            {syncCapabilities.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-3.5 w-3.5" />}
            同步能力
          </Button>
        </div>
      }
      noPadding
    >
      {capabilities.data?.lastError && (
        <div className="px-4 pt-4">
          <Callout tone="warning">{capabilities.data.lastError}</Callout>
        </div>
      )}
      {capabilities.isLoading ? (
        <LoadingState text="加载能力数据..." className="py-8" />
      ) : !capabilities.data?.models.length ? (
        <div className="px-4 pb-4 pt-4">
          <EmptyState title="暂无模型能力数据" description="点击同步按钮获取最新数据，或手动添加模型" />
        </div>
      ) : (
        <div className="scrollbar-thin overflow-x-auto">
          <Table className="min-w-[960px]">
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
                    <TableCell>
                      <div className="max-w-[200px] truncate text-xs font-semibold" title={item.model}>{item.model}</div>
                    </TableCell>
                    <TableCell className="text-xs">{item.displayName || '-'}</TableCell>
                    <TableCell>
                      <Badge tone={sourceTone(item.source)}>{sourceLabel(item.source)}</Badge>
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums" title={item.maxInputTokens ? formatNumber(item.maxInputTokens) : undefined}>{item.maxInputTokens ? formatCompact(item.maxInputTokens) : '—'}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums" title={item.maxOutputTokens ? formatNumber(item.maxOutputTokens) : undefined}>{item.maxOutputTokens ? formatCompact(item.maxOutputTokens) : '—'}</TableCell>
                    <TableCell className="text-right text-xs">
                      {item.supportsPromptCaching === undefined ? '—' : item.supportsPromptCaching ? '支持' : '不支持'}
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground">
                      {item.supportedInputTypes?.length ? item.supportedInputTypes.join(', ') : '—'}
                    </TableCell>
                    <TableCell className="text-right">
                      <div className="flex items-center justify-end gap-1">
                        {isManual ? (
                          <>
                            <Button
                              variant="ghost"
                              size="icon-xs"
                              onClick={() => onEdit(formFromCapability(item, priceMap.get(item.model)))}
                            >
                              <Edit3 className="size-3.5" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-xs"
                              className="text-destructive hover:bg-destructive/10"
                              onClick={() => onDelete(item.model)}
                            >
                              <Trash2 className="size-3.5" />
                            </Button>
                          </>
                        ) : (
                          <span className="text-xs text-muted-foreground/40">-</span>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
      )}
      {capabilities.data?.lastSyncedAt && (
        <div className="border-t border-border px-4 py-2 text-xs text-muted-foreground">
          能力同步: {formatDate(capabilities.data.lastSyncedAt)}
        </div>
      )}
    </SectionCard>
  )
}

// ─── 外部池盈亏面板 ────────────────────────────────────────────────────────────

function ExternalPoolBillingPanel() {
  const summary = useUsageSummary()
  const billing = summary.data?.externalPoolBilling

  if (summary.isLoading) {
    return (
      <SectionCard title="外部池盈亏" description="外部池计费汇总（Uplift / 成本底线）">
        <LoadingState text="加载中..." className="py-6" />
      </SectionCard>
    )
  }

  if (!billing || billing.requests === 0) {
    return (
      <SectionCard title="外部池盈亏" description="外部池计费汇总（Uplift / 成本底线）">
        <EmptyState title="暂无外部池请求" description="没有通过外部池路由的请求记录" className="py-8" />
      </SectionCard>
    )
  }

  const profit = billing.profitUsd ?? 0
  const profitTone = profit > 0 ? 'success' : profit < 0 ? 'error' : 'neutral'

  return (
    <SectionCard
      title="外部池盈亏"
      description="外部池计费汇总（Uplift / 成本底线）"
      icon={profit >= 0 ? <TrendingUp /> : <TrendingDown />}
      actions={
        <Badge tone={profitTone}>
          {profit >= 0 ? '+' : ''}{formatUsd(profit)}
        </Badge>
      }
    >
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {[
          { label: '外部池请求数', value: formatCompact(billing.requests), title: formatNumber(billing.requests) },
          { label: '有计价请求', value: formatCompact(billing.pricedRequests), title: formatNumber(billing.pricedRequests) },
          { label: '无计价请求', value: formatCompact(billing.unpricedRequests), title: formatNumber(billing.unpricedRequests) },
          { label: '原始成本', value: formatUsd(billing.rawCostUsd) },
          { label: '上报费用', value: formatUsd(billing.reportedCostUsd) },
          { label: '可计费费用', value: formatUsd(billing.billableCostUsd) },
          { label: '底线调整', value: formatUsd(billing.costFloorDeltaUsd), tone: billing.costFloorDeltaUsd > 0 ? 'warning' : 'neutral' as const, desc: '实际成本低于底线时按底线计费，底线调整为补的差额' },
          { label: '底线触发次数', value: formatCompact(billing.costFloorAppliedRequests), title: formatNumber(billing.costFloorAppliedRequests), desc: '成本低于配置底线的请求数' },
          ...(billing.profitUsd !== undefined
            ? [{ label: '净盈亏', value: formatUsd(billing.profitUsd), tone: profitTone as 'success' | 'error' | 'neutral' }]
            : []),
        ].map((item) => (
          <div key={item.label} className="rounded-lg border border-border bg-muted/40 px-3 py-2">
            <div className="text-xs text-muted-foreground">{item.label}</div>
            <div className={`mt-0.5 font-mono text-sm font-semibold tabular-nums ${
              item.tone === 'success' ? 'text-success'
              : item.tone === 'error' ? 'text-destructive'
              : item.tone === 'warning' ? 'text-warning'
              : 'text-foreground'
            }`} title={'title' in item ? item.title : undefined}>
              {item.value}
            </div>
            {'desc' in item && item.desc && (
              <div className="mt-0.5 text-[0.68rem] leading-4 text-muted-foreground/70">{item.desc}</div>
            )}
          </div>
        ))}
      </div>
    </SectionCard>
  )
}

// ─── 模型价格目录 ──────────────────────────────────────────────────────────────

function ModelPricingTable({
  onEdit,
  onDelete,
}: {
  onEdit: (form: ManualModelForm) => void
  onDelete: (model: string) => void
}) {
  const pricing = useModelPricing()
  const capabilities = useModelCapabilities()
  const syncPricing = useSyncModelPricing()
  const syncCapabilities = useSyncModelCapabilities()

  const priceMap = useMemo(() => pricingByModel(pricing.data), [pricing.data])

  // Merge capabilities + pricing into unified list
  const rows = useMemo(() => {
    const capModels = capabilities.data?.models ?? []
    const priceModels = pricing.data?.models ?? []

    // Start from capabilities, attach pricing
    const fromCaps = capModels.map((cap) => ({
      model: cap.model,
      displayName: cap.displayName,
      source: cap.source,
      priceItem: priceMap.get(cap.model),
      cap,
    }))

    // Add pricing-only rows (no capability record)
    const capSet = new Set(capModels.map((c) => c.model))
    const pricingOnly = priceModels
      .filter((p) => !capSet.has(p.model))
      .map((p) => ({
        model: p.model,
        displayName: p.model,
        source: p.source,
        priceItem: p,
        cap: null as ModelCapabilityItem | null,
      }))

    return [...fromCaps, ...pricingOnly].sort((a, b) => a.model.localeCompare(b.model))
  }, [capabilities.data?.models, pricing.data?.models, priceMap])

  const pricingStatus = pricing.data
  const capStatus = capabilities.data

  const pricingDesc = [
    `${rows.length} 个模型`,
    pricingStatus?.available === false ? '价格源不可用' : null,
    pricingStatus?.source ? `来源 ${pricingStatus.source}` : null,
    pricingStatus?.sourceUrl ? pricingStatus.sourceUrl : null,
  ]
    .filter(Boolean)
    .join(' · ')

  return (
    <SectionCard
      title="模型价格目录"
      description={pricingDesc}
      icon={<DollarSign />}
      actions={
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              syncCapabilities.mutate(undefined, {
                onSuccess: () => toast.success('模型能力已同步'),
                onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
              })
            }}
            disabled={syncCapabilities.isPending}
          >
            {syncCapabilities.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-3.5 w-3.5" />}
            同步能力
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              syncPricing.mutate(undefined, {
                onSuccess: () => toast.success('模型价格已同步'),
                onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
              })
            }}
            disabled={syncPricing.isPending}
          >
            {syncPricing.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-3.5 w-3.5" />}
            同步价格
          </Button>
        </div>
      }
      noPadding
    >
      {/* 状态提示 */}
      {(pricingStatus?.lastError || capStatus?.lastError) && (
        <div className="px-4 pt-4">
          <Callout tone="warning">
            {pricingStatus?.lastError && <div>价格同步错误: {pricingStatus.lastError}</div>}
            {capStatus?.lastError && <div>能力同步错误: {capStatus.lastError}</div>}
          </Callout>
        </div>
      )}

      {pricing.isLoading || capabilities.isLoading ? (
        <LoadingState text="加载模型数据..." className="py-8" />
      ) : rows.length === 0 ? (
        <div className="px-4 pb-4 pt-4">
          <EmptyState title="暂无模型数据" description="点击同步按钮获取最新数据，或手动添加模型" />
        </div>
      ) : (
        <div className="scrollbar-thin overflow-x-auto">
          <Table className="min-w-[860px]">
            <TableHeader>
              <TableRow>
                <TableHead>模型</TableHead>
                <TableHead>来源</TableHead>
                <TableHead className="text-right">输入单价</TableHead>
                <TableHead className="text-right">输出单价</TableHead>
                <TableHead className="text-right">缓存写入</TableHead>
                <TableHead className="text-right">缓存读取</TableHead>
                <TableHead>上下文</TableHead>
                <TableHead className="text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map(({ model, displayName, source, priceItem, cap }) => (
                <TableRow key={model}>
                  <TableCell>
                    <div className="max-w-[200px] truncate text-xs font-semibold" title={model}>
                      {displayName !== model ? displayName : model}
                    </div>
                    {displayName !== model && (
                      <div className="truncate font-mono text-[0.62rem] text-muted-foreground/60 max-w-[200px]">
                        {model}
                      </div>
                    )}
                  </TableCell>
                  <TableCell>
                    <Badge tone={sourceTone(source)}>{sourceLabel(source)}</Badge>
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs tabular-nums">
                    {priceItem ? formatPricePerMillion(priceItem.pricing.inputCostPerToken * 1_000_000) : <span className="text-muted-foreground/40">—</span>}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs tabular-nums">
                    {priceItem ? formatPricePerMillion(priceItem.pricing.outputCostPerToken * 1_000_000) : <span className="text-muted-foreground/40">—</span>}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs tabular-nums">
                    {priceItem ? formatPricePerMillion(priceItem.pricing.cacheCreationInputTokenCost * 1_000_000) : <span className="text-muted-foreground/40">—</span>}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs tabular-nums">
                    {priceItem ? formatPricePerMillion(priceItem.pricing.cacheReadInputTokenCost * 1_000_000) : <span className="text-muted-foreground/40">—</span>}
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground" title={cap?.maxInputTokens || cap?.maxOutputTokens ? `${cap?.maxInputTokens ? `${formatNumber(cap.maxInputTokens)} in` : ''}${cap?.maxOutputTokens ? ` / ${formatNumber(cap.maxOutputTokens)} out` : ''}` : undefined}>
                    {cap?.maxInputTokens ? `${formatCompact(cap.maxInputTokens)} in` : '—'}
                    {cap?.maxOutputTokens ? ` / ${formatCompact(cap.maxOutputTokens)} out` : ''}
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex items-center justify-end gap-1">
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        onClick={() => {
                          const form = cap
                            ? formFromCapability(cap, priceItem)
                            : {
                                model,
                                displayName: displayName ?? '',
                                description: '',
                                maxInputTokens: '',
                                maxOutputTokens: '',
                                supportsPromptCaching: false,
                                supportedInputTypes: { TEXT: true, IMAGE: false },
                                inputCostPerMillion: priceItem ? String(priceItem.pricing.inputCostPerToken * 1_000_000) : '',
                                outputCostPerMillion: priceItem ? String(priceItem.pricing.outputCostPerToken * 1_000_000) : '',
                                cacheCreationInputCostPerMillion: priceItem ? String(priceItem.pricing.cacheCreationInputTokenCost * 1_000_000) : '',
                                cacheReadInputCostPerMillion: priceItem ? String(priceItem.pricing.cacheReadInputTokenCost * 1_000_000) : '',
                                clearPricing: false,
                                includePricing: Boolean(priceItem),
                              }
                          onEdit(form)
                        }}
                      >
                        <Edit3 className="size-3.5" />
                      </Button>
                      {source === 'manual' && (
                        <Button
                          variant="ghost"
                          size="icon-xs"
                          className="text-destructive hover:bg-destructive/10"
                          onClick={() => onDelete(model)}
                        >
                          <Trash2 className="size-3.5" />
                        </Button>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      {/* 底部元信息 */}
      {(pricingStatus?.lastSyncedAt || capStatus?.lastSyncedAt) && (
        <div className="border-t border-border px-4 py-2 text-xs text-muted-foreground flex flex-wrap gap-3">
          {pricingStatus?.lastSyncedAt && <span>价格同步: {formatDate(pricingStatus.lastSyncedAt)}</span>}
          {capStatus?.lastSyncedAt && <span>能力同步: {formatDate(capStatus.lastSyncedAt)}</span>}
        </div>
      )}
    </SectionCard>
  )
}

// ─── 主页 ──────────────────────────────────────────────────────────────────────

export function CostPage() {
  const [manualOpen, setManualOpen] = useState(false)
  const [editingForm, setEditingForm] = useState<ManualModelForm | null>(null)

  const pricing = useModelPricing()
  const capabilities = useModelCapabilities()
  const summary = useUsageSummary()
  const deleteManual = useDeleteManualModel()
  const confirm = useConfirm()

  const totalModels = capabilities.data?.modelCount ?? pricing.data?.modelCount ?? 0
  const pricedModels = pricing.data?.modelCount ?? 0
  const manualModels = (pricing.data?.models ?? []).filter((m) => m.source === 'manual').length

  const handleDelete = async (model: string) => {
    const ok = await confirm({
      title: '删除手动模型',
      message: `确定删除手动模型「${model}」？此操作无法撤销。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!ok) return
    deleteManual.mutate(model, {
      onSuccess: () => toast.success(`已删除 ${model}`),
      onError: (e) => toast.error(`删除失败: ${extractErrorMessage(e)}`),
    })
  }

  const handleEdit = (form: ManualModelForm) => {
    setEditingForm(form)
    setManualOpen(true)
  }

  return (
    <PageContainer>
      <PageHeader
        title="成本"
        subtitle="模型价格目录、外部池盈亏与计费链路"
        actions={
          <Button size="sm" onClick={() => { setEditingForm(null); setManualOpen(true) }}>
            <Plus className="h-3.5 w-3.5" />添加手动模型
          </Button>
        }
      />

      {/* 指标卡 */}
      <StatGrid>
        <StatCard
          title="模型总数"
          value={formatNumber(totalModels)}
          desc={`已知能力${capabilities.data?.available === false ? ' · 不可用' : ''}${capabilities.data?.source ? ` · ${capabilities.data.source}` : ''}${capabilities.data?.lastSyncedAt ? ` · 同步 ${formatDate(capabilities.data.lastSyncedAt)}` : ''}`}
          icon={<DollarSign />}
          tone={capabilities.data?.available === false ? 'warning' : 'primary'}
        />
        <StatCard
          title="有价格模型"
          value={formatNumber(pricedModels)}
          desc={`可用于计费${pricing.data?.available === false ? ' · 价格不可用' : ''}${pricing.data?.lastSyncedAt ? ` · 同步 ${formatDate(pricing.data.lastSyncedAt)}` : ''}`}
          tone={pricing.data?.available === false ? 'warning' : 'success'}
        />
        <StatCard
          title="手动模型"
          value={formatNumber(manualModels)}
          desc="手动维护"
          tone={manualModels > 0 ? 'warning' : 'default'}
        />
        <StatCard
          title="估算总费用"
          value={formatUsd(summary.data?.totalEstimatedCostUsd ?? 0)}
          desc={`有计价 ${formatCompact(summary.data?.pricedRequests ?? 0)} 次`}
          tone="primary"
        />
      </StatGrid>

      {/* 模型能力目录 */}
      <ModelCapabilitiesTable onAdd={() => { setEditingForm(null); setManualOpen(true) }} onEdit={handleEdit} onDelete={handleDelete} />

      {/* 外部池盈亏 */}
      <ExternalPoolBillingPanel />

      {/* 模型价格目录 */}
      <ModelPricingTable onEdit={handleEdit} onDelete={handleDelete} />

      {/* 手动模型 Modal */}
      <ManualModelModal
        open={manualOpen}
        initialForm={editingForm}
        onClose={() => { setManualOpen(false); setEditingForm(null) }}
      />
    </PageContainer>
  )
}
