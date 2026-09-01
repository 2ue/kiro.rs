import type { ReactNode } from 'react'
import { DollarSign } from 'lucide-react'
import { Badge, Button } from '@/components/ui'
import { SectionCard } from '@/components/patterns'
import { formatCompact, formatMeteringUsage, formatNumber, formatUsdDetailed, formatUsdFixed2 } from '@/lib/format'
import { cn } from '@/lib/utils'
import type {
  ExternalPoolBilling,
  ExternalPoolUsageSnapshot,
  UsageExternalPoolBillingByPool,
  UsageExternalPoolBillingSummary,
  UsageRecord,
} from '@/types/api'
import { billingDeltaBadgeTone, billingDeltaTextClass, billingDeltaTone } from './usage-helpers'

export interface UsageCostModel {
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage?: number
  pricingAvailable?: boolean
  pricingModel?: string
  externalPoolBilling?: ExternalPoolBilling
}

export function usageRecordCostModel(record: UsageRecord): UsageCostModel {
  return {
    estimatedCostUsd: record.estimatedCostUsd,
    originalCostUsd: record.originalCostUsd,
    kiroMeteringUsage: record.kiroMeteringUsage,
    pricingAvailable: record.pricingAvailable,
    pricingModel: record.pricingModel,
    externalPoolBilling: record.externalPoolBilling,
  }
}

export function simpleUsageCostModel(input: {
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage?: number
  pricingAvailable?: boolean
  pricingModel?: string
}): UsageCostModel {
  return input
}

export function formatUsageSnapshot(snapshot?: ExternalPoolUsageSnapshot): string {
  if (!snapshot) return '-'
  return [
    `输入 ${formatNumber(snapshot.inputTokens)}`,
    `输出 ${formatNumber(snapshot.outputTokens)}`,
    `读 ${formatNumber(snapshot.cacheReadInputTokens)}`,
    `写 ${formatNumber(snapshot.cacheCreationInputTokens)}`,
  ].join(' / ')
}

function CostMetric({
  label,
  value,
  detail,
  tone = 'default',
}: {
  label: string
  value: ReactNode
  detail?: ReactNode
  tone?: 'default' | 'primary' | 'warning' | 'success' | 'error'
}) {
  const cls = {
    default: 'text-foreground',
    primary: 'text-primary',
    warning: 'text-warning',
    success: 'text-success',
    error: 'text-destructive',
  }[tone]
  return (
    <div className="rounded-lg bg-muted/30 p-3">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className={cn('mt-1 font-mono text-sm font-semibold tabular-nums', cls)}>{value}</div>
      {detail && <div className="mt-1 text-xs text-muted-foreground/60">{detail}</div>}
    </div>
  )
}

function costMultiplier(rawCostUsd: number, shapedCostUsd: number): string {
  if (!(rawCostUsd > 0) || !(shapedCostUsd > 0)) return '-'
  return (rawCostUsd / shapedCostUsd).toFixed(3)
}

function costDeltaPercent(deltaUsd: number, rawCostUsd: number): string {
  if (!(rawCostUsd > 0)) return '-'
  return `${((deltaUsd / rawCostUsd) * 100).toFixed(2)}%`
}

export function UsageCostInline({
  model,
  onViewDetail,
}: {
  model: UsageCostModel
  onViewDetail?: () => void
}) {
  const pricingLabel = model.pricingAvailable
    ? model.pricingModel || '已计价'
    : '未计价'

  return (
    <div className="text-right font-mono text-xs tabular-nums">
      {onViewDetail ? (
        <Button
          variant="link"
          size="xs"
          className="h-auto p-0 font-mono text-xs font-semibold tabular-nums"
          onClick={onViewDetail}
          title="查看计费明细"
        >
          {formatUsdDetailed(model.estimatedCostUsd)}
        </Button>
      ) : (
        <div className="font-semibold">{formatUsdDetailed(model.estimatedCostUsd)}</div>
      )}
      <div className="text-[0.62rem] text-warning">
        原始 {formatUsdDetailed(model.originalCostUsd)}
      </div>
      <div className="text-[0.62rem] text-muted-foreground/60">{pricingLabel}</div>
      {typeof model.kiroMeteringUsage === 'number' && (
        <div className="text-[0.62rem] text-muted-foreground/60">
          Kiro {formatMeteringUsage(model.kiroMeteringUsage)}
        </div>
      )}
    </div>
  )
}

export function UsageCostTiles({
  model,
  className,
  showKiro = true,
}: {
  model: UsageCostModel
  className?: string
  showKiro?: boolean
}) {
  const pricingLabel = model.pricingAvailable
    ? `是（${model.pricingModel || 'priced'}）`
    : '否'

  return (
    <div className={cn('grid grid-cols-2 gap-2 sm:grid-cols-3', className)}>
      <CostMetric label="估算费用" value={formatUsdDetailed(model.estimatedCostUsd)} tone={model.estimatedCostUsd > 0 ? 'warning' : 'default'} />
      <CostMetric label="原始计费" value={formatUsdDetailed(model.originalCostUsd)} tone={model.originalCostUsd > 0 ? 'warning' : 'default'} />
      {showKiro && typeof model.kiroMeteringUsage === 'number' && (
        <CostMetric label="Kiro计量" value={formatMeteringUsage(model.kiroMeteringUsage)} />
      )}
      <CostMetric label="有定价" value={pricingLabel} />
    </div>
  )
}

export function UsageCostBreakdown({
  model,
}: {
  model: UsageCostModel
}) {
  const billing = model.externalPoolBilling
  const shapedCost = billing?.shapedCostUsd ?? billing?.reportedCostUsd ?? model.estimatedCostUsd
  const upliftedCost = billing?.upliftedCostUsd ?? billing?.reportedCostUsd ?? billing?.billableCostUsd ?? model.estimatedCostUsd
  const delta = billing
    ? billing.profitUsd ?? (upliftedCost - (billing.rawCostUsd || 0))
    : model.estimatedCostUsd - model.originalCostUsd
  const deltaTone = billingDeltaTone(delta)

  return (
    <div>
      <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">计费口径</div>
      {billing ? (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          <CostMetric
            label="上游原始 usage 成本"
            value={formatUsdDetailed(billing.rawCostUsd)}
            detail={formatUsageSnapshot(billing.rawUsage)}
          />
          <CostMetric
            label="展示计费"
            value={formatUsdDetailed(shapedCost)}
            detail={billing.usageProjectionApplied ? '已按路径整理' : '透传上游'}
          />
          <CostMetric
            label="补偿后计费"
            value={formatUsdDetailed(upliftedCost)}
            detail={formatUsageSnapshot(billing.reportedUsage)}
          />
          <CostMetric
            label="上报费用 / 可计费"
            value={`${formatUsdDetailed(billing.reportedCostUsd)} / ${formatUsdDetailed(billing.billableCostUsd)}`}
          />
          <CostMetric
            label="计费差额"
            value={`${delta >= 0 ? '+' : ''}${formatUsdDetailed(delta)}`}
            tone={deltaTone === 'loss' ? 'error' : deltaTone === 'profit' ? 'warning' : 'default'}
            detail="补偿后计费 - 上游原始成本"
          />
          <CostMetric
            label="计价模型 / 用量模式"
            value={billing.pricingAvailable ? billing.pricingModel || 'priced' : 'unpriced'}
            detail={billing.usageProjectionApplied ? '已按路径整理' : '透传上游'}
          />
        </div>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          <CostMetric
            label="原始计费"
            value={formatUsdDetailed(model.originalCostUsd)}
            detail="按原始 usage 估算"
          />
          <CostMetric
            label="展示计费"
            value={formatUsdDetailed(model.estimatedCostUsd)}
            detail="按下游展示 usage 估算"
          />
          <CostMetric
            label="计费差额"
            value={`${delta >= 0 ? '+' : ''}${formatUsdDetailed(delta)}`}
            tone={deltaTone === 'loss' ? 'error' : deltaTone === 'profit' ? 'warning' : 'default'}
            detail="展示计费 - 原始计费"
          />
          <CostMetric
            label="Kiro计量"
            value={typeof model.kiroMeteringUsage === 'number' ? formatMeteringUsage(model.kiroMeteringUsage) : '-'}
          />
        </div>
      )}
    </div>
  )
}

export function ExternalPoolBillingPanel({
  billing,
  billingByPool,
}: {
  billing: UsageExternalPoolBillingSummary
  billingByPool: UsageExternalPoolBillingByPool[]
}) {
  const shapedCost = billing.shapedCostUsd ?? billing.reportedCostUsd ?? 0
  const upliftedCost = billing.upliftedCostUsd ?? billing.reportedCostUsd ?? billing.billableCostUsd ?? 0
  const delta = billing.profitUsd ?? (upliftedCost - (billing.rawCostUsd || 0))
  const deltaRatio = billing.rawCostUsd > 0 ? delta / billing.rawCostUsd : 0
  const multiplier = costMultiplier(billing.rawCostUsd, shapedCost)
  const deltaTone = billingDeltaTone(delta)
  const hasNegativeDelta = deltaTone === 'loss'
  const hasPositiveDelta = deltaTone === 'profit'
  const visiblePools = billingByPool.filter((pool) => pool.requests > 0).slice(0, 20)

  return (
    <SectionCard
      title="外部池成本拆分"
      description="区分上游原始成本、整形后成本、补偿后成本，并计算每个外部池的差额占比和原始/整形倍率"
      icon={<DollarSign />}
      actions={
        <Badge tone={billingDeltaBadgeTone(deltaTone)}>
          {hasNegativeDelta ? `差额 -${formatUsdFixed2(Math.abs(delta))}` : hasPositiveDelta ? `差额 +${formatUsdFixed2(delta)}` : '差额 $0.00'}
        </Badge>
      }
    >
      <div className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          <CostMetric
            label="外部池请求"
            value={<span title={formatNumber(billing.requests)}>{formatCompact(billing.requests)}</span>}
            detail={<>可计价 <span title={formatNumber(billing.pricedRequests)}>{formatCompact(billing.pricedRequests)}</span> / 未计价 <span title={formatNumber(billing.unpricedRequests)}>{formatCompact(billing.unpricedRequests)}</span></>}
          />
          <CostMetric
            label="上游原始成本"
            value={formatUsdFixed2(billing.rawCostUsd)}
            detail="按外部上游返回 usage 估算"
          />
          <CostMetric
            label="展示计费"
            value={formatUsdFixed2(shapedCost)}
            detail="按当前展示规则计算"
          />
          <CostMetric
            label="补偿后计费"
            value={formatUsdFixed2(upliftedCost)}
            detail={<span className={billingDeltaTextClass(deltaTone)}>差额 = 补偿后 - 上游原始：{delta >= 0 ? '+' : ''}{formatUsdFixed2(delta)}</span>}
          />
        </div>

        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-2 text-xs">
            <span className="truncate font-medium text-foreground/75">总体差额占上游原始成本</span>
            <span className="shrink-0 font-mono text-muted-foreground">
              {delta >= 0 ? '+' : ''}{formatUsdFixed2(delta)} · {costDeltaPercent(delta, billing.rawCostUsd)} · 原始/整形倍率 {multiplier}
            </span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-muted">
            <div
              className={cn('h-full rounded-full', hasNegativeDelta ? 'bg-warning/80' : 'bg-success/80')}
              style={{ width: `${Math.min(100, Math.abs(deltaRatio) * 100)}%` }}
            />
          </div>
        </div>

        <div className="pt-1">
          <div className="mb-2 flex items-center justify-between gap-2">
            <div className="text-xs font-semibold text-foreground/70">各外部池成本与差额</div>
            <div className="text-[0.68rem] text-muted-foreground/45">按当前时间窗口聚合</div>
          </div>
          {visiblePools.length === 0 ? (
            <div className="rounded-lg bg-muted/30 p-3 text-sm text-muted-foreground/60">
              当前窗口没有外部池计费样本；发生外部池请求后，这里会按池列出原始成本、差额占比和原始/整形倍率。
            </div>
          ) : (
            <div className="scrollbar-thin overflow-x-auto rounded-lg bg-card">
              <table className="w-full min-w-[640px] text-xs">
                <thead>
                  <tr className="bg-muted/40 text-muted-foreground">
                    <th className="px-3 py-2 text-left font-medium">外部池</th>
                    <th className="px-3 py-2 text-right font-medium">请求</th>
                    <th className="px-3 py-2 text-right font-medium">上游原始成本</th>
                    <th className="px-3 py-2 text-right font-medium">展示计费</th>
                    <th className="px-3 py-2 text-right font-medium">补偿后</th>
                    <th className="px-3 py-2 text-right font-medium">差额占原始</th>
                    <th className="px-3 py-2 text-right font-medium">原始/整形倍率</th>
                    <th className="px-3 py-2 text-right font-medium">未计价</th>
                    <th className="px-3 py-2 text-right font-medium">兜底</th>
                  </tr>
                </thead>
                <tbody>
                  {visiblePools.map((pool) => {
                    const poolDelta = pool.profitUsd ?? ((pool.upliftedCostUsd ?? pool.reportedCostUsd ?? 0) - pool.rawCostUsd)
                    const poolShapedCost = pool.shapedCostUsd ?? pool.reportedCostUsd ?? 0
                    const poolTone = billingDeltaTone(poolDelta)
                    return (
                      <tr key={pool.poolId} className="bg-card transition-colors hover:bg-muted/30">
                        <td className="px-3 py-2">
                          <div className="max-w-[200px] truncate font-medium" title={pool.poolName}>{pool.poolName}</div>
                          <div className="font-mono text-[0.62rem] text-muted-foreground/45">#{pool.poolId}</div>
                        </td>
                        <td className="px-3 py-2 text-right font-mono" title={formatNumber(pool.requests)}>{formatCompact(pool.requests)}</td>
                        <td className="px-3 py-2 text-right font-mono">{formatUsdFixed2(pool.rawCostUsd)}</td>
                        <td className="px-3 py-2 text-right font-mono">{formatUsdFixed2(pool.shapedCostUsd ?? pool.reportedCostUsd)}</td>
                        <td className="px-3 py-2 text-right font-mono">{formatUsdFixed2(pool.upliftedCostUsd ?? pool.reportedCostUsd)}</td>
                        <td className={cn('px-3 py-2 text-right font-mono', billingDeltaTextClass(poolTone))}>
                          <div>{poolDelta >= 0 ? '+' : ''}{formatUsdFixed2(poolDelta)}</div>
                          <div className="text-[0.62rem]" title="当前账号差额 ÷ 当前账号上游原始成本">
                            {costDeltaPercent(poolDelta, pool.rawCostUsd)}
                          </div>
                        </td>
                        <td className="px-3 py-2 text-right font-mono" title="上游原始成本 ÷ 整形后展示成本；原始100、整形200时为0.500">
                          {costMultiplier(pool.rawCostUsd, poolShapedCost)}
                        </td>
                        <td className="px-3 py-2 text-right font-mono" title={formatNumber(pool.unpricedRequests)}>{formatCompact(pool.unpricedRequests)}</td>
                        <td className="px-3 py-2 text-right font-mono" title={formatNumber(pool.costFloorAppliedRequests)}>{formatCompact(pool.costFloorAppliedRequests)}</td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>
    </SectionCard>
  )
}
