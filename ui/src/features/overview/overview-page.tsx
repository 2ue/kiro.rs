import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  Clock3,
  DollarSign,
  ExternalLink,
  Layers3,
  RefreshCw,
  TrendingUp,
  Wallet,
  Zap,
} from 'lucide-react'
import { getExternalPools } from '@/api/credentials'
import {
  useUsageOverviewExternal,
  useUsageOverviewLocal,
  useUsageOverviewRankings,
  useUsageOverviewSeries,
  useUsageOverviewSummary,
} from '@/hooks/use-usage'
import {
  useCredentials,
  useCredentialAccountInfo,
  useCredentialRuntime,
} from '@/hooks/use-credentials'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { formatCompact, formatCredits, formatDate, formatNumber, formatPercent, formatUsdFixed2 } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import { credentialCreditStatus, credentialLabel, mapById } from '@/features/credentials/credential-utils'
import { billingDeltaTextClass, billingDeltaTone } from '@/features/usage/usage-helpers'
import type {
  CredentialAccountInfo,
  CredentialStatusItem,
  ExternalPool,
  UsageOverviewMetrics,
  UsageOverviewRankRow,
  UsageOverviewSeriesPoint,
} from '@/types/api'
import {
  Callout,
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Input,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Tooltip,
} from '@/components/ui'

const TIMEZONE = 'Asia/Shanghai'
const AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:overview'

type PresetRange = 'today' | 'yesterday' | 'last24h' | 'last7d' | 'last30d' | 'thisMonth'
type TrendMetric = 'requests' | 'estimatedCostUsd' | 'originalCostUsd' | 'kiroMeteringUsage' | 'tokens'
type OverviewTab = 'summary' | 'local' | 'external' | 'rankings'
type RankingDimension = 'models' | 'accounts' | 'keys' | 'paths' | 'errors'
type TokenUsageMetrics = Pick<UsageOverviewMetrics, 'inputTokens' | 'outputTokens' | 'cacheReadInputTokens' | 'cacheCreationInputTokens'>

type LocalAccountOverviewRow = {
  credential: CredentialStatusItem
  credentialId: number
  label: string
  metrics: UsageOverviewMetrics
  accountInfo?: CredentialAccountInfo
  creditEstimateBlocked: boolean
  creditEstimateBlockedReason?: string
  estimatedRemainingCostUsd?: number
}

type ExternalUsagePool = {
  poolId: number
  poolName: string
  metrics: UsageOverviewMetrics
  rawCostUsd: number
  shapedCostUsd: number
  upliftedCostUsd: number
  reportedCostUsd: number
  billableCostUsd: number
  profitUsd: number
  costFloorDeltaUsd: number
  costFloorAppliedRequests: number
}

const EMPTY_OVERVIEW_METRICS: UsageOverviewMetrics = {
  requests: 0,
  successRequests: 0,
  errorRequests: 0,
  errorRate: 0,
  inputTokens: 0,
  outputTokens: 0,
  cacheReadInputTokens: 0,
  cacheCreationInputTokens: 0,
  estimatedCostUsd: 0,
  originalCostUsd: 0,
  kiroMeteringUsage: 0,
  pricedRequests: 0,
  unpricedRequests: 0,
  averageDurationMs: 0,
}

const PRESETS: Array<{ key: PresetRange; label: string }> = [
  { key: 'today', label: '今天' },
  { key: 'yesterday', label: '昨天' },
  { key: 'last24h', label: '24 小时' },
  { key: 'last7d', label: '7 天' },
  { key: 'last30d', label: '30 天' },
  { key: 'thisMonth', label: '本月' },
]

const TREND_METRICS: Array<{ key: TrendMetric; label: string }> = [
  { key: 'requests', label: '请求' },
  { key: 'estimatedCostUsd', label: '估算计费' },
  { key: 'originalCostUsd', label: '原始计费' },
  { key: 'kiroMeteringUsage', label: 'Kiro 积分' },
  { key: 'tokens', label: 'Token' },
]

function localDateTimeValue(value?: string | null): string {
  if (!value) return ''
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

function localDateTimeToIso(value: string): string | undefined {
  if (!value.trim()) return undefined
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString()
}

function rangeLabel(range: { key?: string | null; from: string; to: string }): string {
  if (range.key) return PRESETS.find((item) => item.key === range.key)?.label ?? range.key
  return `${formatDate(range.from)} - ${formatDate(range.to)}`
}

function metricsShare(part: number, total: number): number {
  return total > 0 ? part / total : 0
}

function costMultiplier(rawCostUsd: number, shapedCostUsd: number): string {
  if (!(rawCostUsd > 0) || !(shapedCostUsd > 0)) return '-'
  return (rawCostUsd / shapedCostUsd).toFixed(3)
}

function costDeltaPercent(deltaUsd: number, rawCostUsd: number): string {
  if (!(rawCostUsd > 0)) return '-'
  return `${((deltaUsd / rawCostUsd) * 100).toFixed(2)}%`
}

function localCostDiffInfo(estimatedCostUsd: number, originalCostUsd: number) {
  const deltaUsd = estimatedCostUsd - originalCostUsd
  const percentText = originalCostUsd > 0
    ? ` (${deltaUsd >= 0 ? '+' : ''}${formatPercent(deltaUsd / originalCostUsd)})`
    : ''
  return {
    deltaUsd,
    text: `${deltaUsd >= 0 ? '+' : '-'}${formatUsdFixed2(Math.abs(deltaUsd))}`,
    percentText,
    textClass: deltaUsd === 0 ? 'text-muted-foreground' : 'text-info',
  }
}

function creditCostRate(costUsd: number, usedCredits: number): number | undefined {
  if (!(usedCredits > 0)) return undefined
  return costUsd / usedCredits
}

function formatCreditCostRate(value: number | undefined): string {
  if (value === undefined) return '-'
  return `${new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: 3,
    maximumFractionDigits: 3,
  }).format(value)}/积分`
}

function mergeCredentialRuntime(base: CredentialStatusItem, runtime?: Partial<CredentialStatusItem>): CredentialStatusItem {
  if (!runtime) return base
  return {
    ...base,
    ...runtime,
    maxConcurrentRequests: runtime.maxConcurrentRequests ?? base.maxConcurrentRequests,
    rpm: runtime.rpm ?? base.rpm,
    warmupRemaining: runtime.warmupRemaining ?? base.warmupRemaining,
  }
}

function totalTokenUsage(metrics: TokenUsageMetrics): number {
  return metrics.inputTokens + metrics.outputTokens + metrics.cacheReadInputTokens + metrics.cacheCreationInputTokens
}

function seriesMetricValue(point: UsageOverviewSeriesPoint, scope: 'all' | 'local' | 'external', key: TrendMetric): number {
  const metrics = point[scope]
  if (key === 'tokens') return totalTokenUsage(metrics)
  return metrics[key]
}

function trendDisplayValue(point: UsageOverviewSeriesPoint, key: TrendMetric, showLocal: boolean, showExternal: boolean): number {
  const local = seriesMetricValue(point, 'local', key)
  const external = seriesMetricValue(point, 'external', key)
  return (showLocal ? local : 0) + (showExternal ? external : 0)
}

function trendTooltip(point: UsageOverviewSeriesPoint, key: TrendMetric, showLocal: boolean, showExternal: boolean): string {
  const displayed = trendDisplayValue(point, key, showLocal, showExternal)
  const local = seriesMetricValue(point, 'local', key)
  const external = seriesMetricValue(point, 'external', key)
  return `${point.label} · 当前显示 ${formatTrendValue(displayed, key)} · 本地 ${formatTrendValue(local, key)} · 外部 ${formatTrendValue(external, key)}`
}

function formatTrendValue(value: number, key: TrendMetric): string {
  if (key === 'estimatedCostUsd' || key === 'originalCostUsd') return formatUsdFixed2(value)
  return formatCompact(value)
}

function trendAxisTicks(max: number): number[] {
  return [1, 0.75, 0.5, 0.25, 0].map((ratio) => max * ratio)
}

function TrendTooltipContent({
  point,
  metric,
  showLocal,
  showExternal,
}: {
  point: UsageOverviewSeriesPoint
  metric: TrendMetric
  showLocal: boolean
  showExternal: boolean
}) {
  const displayed = trendDisplayValue(point, metric, showLocal, showExternal)
  const local = seriesMetricValue(point, 'local', metric)
  const external = seriesMetricValue(point, 'external', metric)

  return (
    <div className="w-44 space-y-2 font-normal">
      <div className="font-semibold text-secondary-foreground">{point.label}</div>
      <div className="space-y-1 font-mono tabular-nums">
        <div className="flex items-center justify-between gap-3">
          <span className="text-secondary-foreground/70">当前显示</span>
          <span>{formatTrendValue(displayed, metric)}</span>
        </div>
        <div className={cn('flex items-center justify-between gap-3', !showLocal && 'opacity-45')}>
          <span className="inline-flex items-center gap-1.5 text-secondary-foreground/70">
            <span className="size-2 rounded-full bg-primary" />
            本地
          </span>
          <span>{formatTrendValue(local, metric)}</span>
        </div>
        <div className={cn('flex items-center justify-between gap-3', !showExternal && 'opacity-45')}>
          <span className="inline-flex items-center gap-1.5 text-secondary-foreground/70">
            <span className="size-2 rounded-full bg-info" />
            外部
          </span>
          <span>{formatTrendValue(external, metric)}</span>
        </div>
        <div className="flex items-center justify-between gap-3 border-t border-secondary-foreground/15 pt-1">
          <span className="text-secondary-foreground/70">本地占比</span>
          <span>{formatPercent(metricsShare(local, local + external))}</span>
        </div>
      </div>
    </div>
  )
}

function usageLink(
  navigate: ReturnType<typeof useNavigate>,
  params: { routeKind: 'local_credential' | 'external_pool'; credentialId?: number; externalPoolId?: number; since: string; until: string },
) {
  const search = new URLSearchParams()
  search.set('routeKind', params.routeKind)
  if (params.credentialId) search.set('credentialId', String(params.credentialId))
  if (params.externalPoolId) search.set('externalPoolId', String(params.externalPoolId))
  if (params.since) search.set('since', localDateTimeValue(params.since))
  if (params.until) search.set('until', localDateTimeValue(params.until))
  navigate(`/usage?${search.toString()}`)
}

function OverallSummarySection({ metrics }: { metrics: UsageOverviewMetrics }) {
  return (
    <SectionCard>
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard title="请求数" value={formatCompact(metrics.requests)} valueTitle={formatNumber(metrics.requests)} desc={`${formatCompact(metrics.successRequests)} 成功 · ${formatCompact(metrics.errorRequests)} 错误`} icon={<Activity />} tone="primary" />
        <StatCard title="成功率" value={formatPercent(metricsShare(metrics.successRequests, metrics.requests))} desc={`错误率 ${formatPercent(metrics.errorRate)}`} icon={<TrendingUp />} tone={metrics.errorRate > 0.05 ? 'warning' : 'success'} />
        <TokenSummaryCard metrics={metrics} className="sm:col-span-2" />
        <StatCard title="估算计费" value={formatUsdFixed2(metrics.estimatedCostUsd)} icon={<DollarSign />} tone="primary" />
        <StatCard title="原始计费" value={formatUsdFixed2(metrics.originalCostUsd)} icon={<DollarSign />} tone="warning" />
        <StatCard title="Kiro 积分" value={formatCompact(metrics.kiroMeteringUsage)} valueTitle={formatNumber(metrics.kiroMeteringUsage)} icon={<Zap />} tone="info" />
        <StatCard title="平均耗时" value={`${Math.round(metrics.averageDurationMs)} ms`} desc={`${formatCompact(metrics.pricedRequests)} 个请求已计价`} icon={<Clock3 />} tone="default" />
      </div>
    </SectionCard>
  )
}

function TokenSummaryCard({
  metrics,
  className,
}: {
  metrics: UsageOverviewMetrics
  className?: string
}) {
  const total = totalTokenUsage(metrics)
  return (
    <div className={cn(
      'relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl bg-card p-4 shadow-sm transition hover:shadow-md',
      className,
    )}>
      <span className="absolute left-0 top-4 h-8 w-1 rounded-r-full bg-amber-500" />
      <div className="flex items-start justify-between gap-2 pl-2.5">
        <div className="min-w-0 flex-1">
          <div className="text-[0.72rem] font-semibold text-muted-foreground">Token</div>
          <div className="mt-1 break-words text-2xl font-semibold tracking-tight tabular-nums text-amber-600 dark:text-amber-400" title={formatNumber(total)}>
            {formatCompact(total)}
          </div>
        </div>
        <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-amber-600 dark:text-amber-400 [&_svg]:size-4">
          <Layers3 />
        </div>
      </div>
      <div className="mt-3 grid grid-cols-2 gap-x-4 gap-y-1.5 pl-2.5 text-[0.72rem] leading-4 sm:grid-cols-4">
        <TokenBreakdownItem label="输入" value={metrics.inputTokens} className="text-emerald-600 dark:text-emerald-400" />
        <TokenBreakdownItem label="输出" value={metrics.outputTokens} className="text-violet-600 dark:text-violet-400" />
        <TokenBreakdownItem label="缓存读" value={metrics.cacheReadInputTokens} className="text-sky-600 dark:text-sky-400" />
        <TokenBreakdownItem label="缓存写" value={metrics.cacheCreationInputTokens} className="text-amber-600 dark:text-amber-400" />
      </div>
    </div>
  )
}

function TokenBreakdownItem({
  label,
  value,
  className,
}: {
  label: string
  value: number
  className: string
}) {
  return (
    <div className="min-w-0">
      <div className="text-muted-foreground">{label}</div>
      <div className={cn('truncate font-semibold tabular-nums', className)} title={formatNumber(value)}>
        {formatCompact(value)}
      </div>
    </div>
  )
}

function TrendPanel({
  points,
  metric,
  onMetricChange,
  onDrillDown,
}: {
  points: UsageOverviewSeriesPoint[]
  metric: TrendMetric
  onMetricChange: (metric: TrendMetric) => void
  onDrillDown: (point: UsageOverviewSeriesPoint) => void
}) {
  const [showLocal, setShowLocal] = useState(true)
  const [showExternal, setShowExternal] = useState(true)
  const maxValue = Math.max(
    0,
    ...points.map((point) => trendDisplayValue(point, metric, showLocal, showExternal)),
  )
  const axisMax = Math.max(1, maxValue)
  const plotHeight = 190
  const plotTopPadding = 28
  const chartHeight = plotHeight + plotTopPadding
  const ticks = trendAxisTicks(axisMax)
  const bucketWidth = points.length > 24 ? 64 : points.length > 12 ? 70 : 76
  const plotMinWidth = Math.max(680, points.length * bucketWidth)

  return (
    <SectionCard
      title="趋势"
      icon={<TrendingUp />}
      actions={
        <div className="flex flex-wrap gap-1">
          {TREND_METRICS.map((item) => (
            <Button
              key={item.key}
              size="xs"
              variant={metric === item.key ? 'secondary' : 'ghost'}
              onClick={() => onMetricChange(item.key)}
            >
              {item.label}
            </Button>
          ))}
        </div>
      }
    >
      {points.length === 0 ? (
        <EmptyState title="暂无趋势数据" className="py-8" />
      ) : (
        <div className="flex pt-1">
          <div className="relative w-16 shrink-0 pr-2" style={{ height: chartHeight }}>
            {ticks.map((tick) => {
              const top = plotTopPadding + ((axisMax - tick) / axisMax) * plotHeight
              return (
                <span
                  key={tick}
                  className="absolute right-2 -translate-y-1/2 whitespace-nowrap font-mono text-[0.62rem] tabular-nums text-muted-foreground"
                  style={{ top }}
                >
                  {formatTrendValue(tick, metric)}
                </span>
              )
            })}
          </div>
          <div className="min-w-0 flex-1 overflow-x-auto pb-2">
            <div style={{ minWidth: plotMinWidth }}>
              <div
                className="relative flex-1 border-b border-l border-border/70"
                style={{ height: chartHeight, minWidth: plotMinWidth }}
              >
                {ticks.map((tick) => {
                  const top = plotTopPadding + ((axisMax - tick) / axisMax) * plotHeight
                  return (
                    <span
                      key={tick}
                      className="absolute left-0 right-0 border-t border-dashed border-border/60"
                      style={{ top }}
                    />
                  )
                })}
                <div className="absolute inset-x-3 bottom-0 z-10 flex items-end gap-3" style={{ height: plotHeight }}>
                  {points.map((point) => {
                    const local = seriesMetricValue(point, 'local', metric)
                    const external = seriesMetricValue(point, 'external', metric)
                    const displayed = trendDisplayValue(point, metric, showLocal, showExternal)
                    const localHeight = showLocal && local > 0 ? Math.max(2, (local / axisMax) * plotHeight) : 0
                    const externalHeight = showExternal && external > 0 ? Math.max(2, (external / axisMax) * plotHeight) : 0
                    const totalHeight = Math.min(plotHeight, localHeight + externalHeight)

                    return (
                      <Tooltip
                        key={point.bucketStart}
                        label={<TrendTooltipContent point={point} metric={metric} showLocal={showLocal} showExternal={showExternal} />}
                      >
                        <Button
                          variant="ghost"
                          size="icon-xs"
                          className="group relative flex h-full flex-1 items-end justify-center overflow-visible rounded-sm px-1 pb-0 pt-0 hover:bg-muted/40"
                          title={`${trendTooltip(point, metric, showLocal, showExternal)}，点击查看`}
                          onClick={() => onDrillDown(point)}
                        >
                          <span
                            className="relative flex w-full max-w-12 flex-col items-stretch justify-end"
                            style={{ height: `${totalHeight}px` }}
                          >
                            {displayed > 0 && (
                              <span className="absolute bottom-full left-1/2 mb-1 max-w-16 -translate-x-1/2 truncate rounded-sm bg-background/95 px-1 font-mono text-[0.58rem] font-semibold tabular-nums text-foreground shadow-sm ring-1 ring-border/80">
                                {formatTrendValue(displayed, metric)}
                              </span>
                            )}
                            <span className="w-full rounded-t-sm bg-info/75 transition group-hover:bg-info" style={{ height: `${externalHeight}px` }} />
                            <span className="w-full rounded-b-sm bg-primary/80 transition group-hover:bg-primary" style={{ height: `${localHeight}px` }} />
                          </span>
                        </Button>
                      </Tooltip>
                    )
                  })}
                </div>
              </div>
              <div className="mt-1 flex flex-1 gap-3 px-3" style={{ minWidth: plotMinWidth }}>
                {points.map((point) => (
                  <span key={`${point.bucketStart}-label`} className="flex-1 whitespace-nowrap text-center text-[0.62rem] text-muted-foreground">
                    {point.label}
                  </span>
                ))}
              </div>
            </div>
          </div>
        </div>
      )}
      <div className="mt-3 flex flex-wrap items-center gap-4 text-[0.68rem] text-muted-foreground">
        <Button
          size="xs"
          variant={showLocal ? 'secondary' : 'ghost'}
          className="h-6 gap-1.5 px-2 text-[0.68rem]"
          aria-pressed={showLocal}
          onClick={() => setShowLocal((value) => !value)}
        >
          <span className={cn('size-2 rounded-full', showLocal ? 'bg-primary/80' : 'bg-muted-foreground/30')} />
          本地账号池
        </Button>
        <Button
          size="xs"
          variant={showExternal ? 'secondary' : 'ghost'}
          className="h-6 gap-1.5 px-2 text-[0.68rem]"
          aria-pressed={showExternal}
          onClick={() => setShowExternal((value) => !value)}
        >
          <span className={cn('size-2 rounded-full', showExternal ? 'bg-info/75' : 'bg-muted-foreground/30')} />
          外部池
        </Button>
      </div>
    </SectionCard>
  )
}

function LocalPoolSection({
  metrics,
  accountRows,
  accountsLoading,
  range,
  navigate,
}: {
  metrics: UsageOverviewMetrics
  accountRows: LocalAccountOverviewRow[]
  accountsLoading: boolean
  range: { from: string; to: string }
  navigate: ReturnType<typeof useNavigate>
}) {
  const availableCreditRows = accountRows.filter((account) => (
    !account.creditEstimateBlocked &&
    account.accountInfo?.creditRemaining != null
  ))
  const availableCreditRemaining = availableCreditRows.reduce(
    (sum, account) => sum + (account.accountInfo?.creditRemaining ?? 0),
    0,
  )
  const estimatedRemainingCostUsd = availableCreditRows.reduce(
    (sum, account) => sum + (account.estimatedRemainingCostUsd ?? 0),
    0,
  )
  const unavailableCreditCount = accountRows.filter((account) => account.creditEstimateBlocked).length
  const unqueriedCreditCount = accountRows.filter((account) => (
    !account.creditEstimateBlocked &&
    account.accountInfo?.creditRemaining == null
  )).length
  const localCostDiff = localCostDiffInfo(metrics.estimatedCostUsd, metrics.originalCostUsd)
  const estimatedCreditRate = creditCostRate(metrics.estimatedCostUsd, metrics.kiroMeteringUsage)
  const originalCreditRate = creditCostRate(metrics.originalCostUsd, metrics.kiroMeteringUsage)

  return (
    <SectionCard>
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-6">
        <StatCard title="已消耗积分" value={formatNumber(metrics.kiroMeteringUsage)} valueTitle={formatNumber(metrics.kiroMeteringUsage)} desc="请求级 meteringEvent 消耗，包含已失效账号" icon={<Zap />} tone="info" />
        <StatCard
          title="估算费用"
          value={formatUsdFixed2(metrics.estimatedCostUsd)}
          desc={(
            <span>
              原始计费 {formatUsdFixed2(metrics.originalCostUsd)} ·{' '}
              <span className={localCostDiff.textClass}>
                差值 {localCostDiff.text}{localCostDiff.percentText}
              </span>
            </span>
          )}
          icon={<DollarSign />}
          tone="primary"
        />
        <StatCard
          title="积分转换率"
          value={formatCreditCostRate(estimatedCreditRate)}
          desc={`原始 ${formatCreditCostRate(originalCreditRate)}`}
          icon={<TrendingUp />}
          tone="info"
        />
        <StatCard
          title="剩余积分"
          value={formatCredits(availableCreditRemaining)}
          desc={unavailableCreditCount > 0
            ? `已排除 ${formatNumber(unavailableCreditCount)} 个不可用账号`
            : unqueriedCreditCount > 0
              ? `${formatNumber(unqueriedCreditCount)} 个账号未查询余额`
              : '当前可用账号余额'}
          icon={<Wallet />}
          tone="success"
        />
        <StatCard
          title="预估估算费用"
          value={formatUsdFixed2(estimatedRemainingCostUsd)}
          desc="估算计费 / 已消耗积分 × 剩余积分"
          icon={<DollarSign />}
          tone="success"
        />
        <StatCard title="错误率" value={formatPercent(metrics.errorRate)} desc={`${formatCompact(metrics.errorRequests)} 个错误请求`} icon={<AlertTriangle />} tone={metrics.errorRate > 0.05 ? 'error' : metrics.errorRate > 0 ? 'warning' : 'success'} />
      </div>
      <div className="mt-4 flex justify-end">
        <Button size="xs" variant="outline" onClick={() => navigate('/credentials')}>
          账号管理 <ExternalLink className="size-3.5" />
        </Button>
      </div>
      {accountsLoading ? (
        <LoadingState text="加载本地账号数据..." className="mt-3 py-8" />
      ) : accountRows.length === 0 ? (
        <EmptyState title="暂无本地账号" className="mt-3 py-8" />
      ) : (
        <div className="mt-2 overflow-x-auto">
          <Table className="min-w-[1040px]">
            <TableHeader>
              <TableRow>
                <TableHead>账号</TableHead>
                <TableHead className="text-right">请求</TableHead>
                <TableHead className="text-right">已消耗积分</TableHead>
                <TableHead className="text-right">估算费用</TableHead>
                <TableHead className="text-right">积分转换率</TableHead>
                <TableHead className="text-right">剩余积分</TableHead>
                <TableHead className="text-right">预估估算费用</TableHead>
                <TableHead className="text-right">错误率</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {accountRows.map((account) => {
                const accountCostDiff = localCostDiffInfo(account.metrics.estimatedCostUsd, account.metrics.originalCostUsd)
                const accountEstimatedRate = creditCostRate(account.metrics.estimatedCostUsd, account.metrics.kiroMeteringUsage)
                const accountOriginalRate = creditCostRate(account.metrics.originalCostUsd, account.metrics.kiroMeteringUsage)
                return (
                  <TableRow key={account.credentialId} className={account.credential.disabled ? 'opacity-65' : undefined}>
                    <TableCell>
                      <Button
                        variant="link"
                        size="xs"
                        className="h-auto max-w-[260px] justify-start truncate px-0 text-left"
                        title={`账号 #${account.credentialId} · 查看 Usage 明细`}
                        onClick={() => usageLink(navigate, {
                          routeKind: 'local_credential',
                          credentialId: account.credentialId,
                          since: range.from,
                          until: range.to,
                        })}
                      >
                        #{account.credentialId} {account.label}
                      </Button>
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs">{formatNumber(account.metrics.requests)}</TableCell>
                    <TableCell className="text-right font-mono text-xs">{formatNumber(account.metrics.kiroMeteringUsage)}</TableCell>
                    <TableCell className="text-right font-mono text-xs">
                      <div>{formatUsdFixed2(account.metrics.estimatedCostUsd)}</div>
                      <div className="text-[0.62rem] text-muted-foreground">原始 {formatUsdFixed2(account.metrics.originalCostUsd)}</div>
                      <div className={cn('text-[0.62rem]', accountCostDiff.textClass)} title="估算费用 - 原始计费">
                        差值 {accountCostDiff.text}{accountCostDiff.percentText}
                      </div>
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs">
                      <div>{formatCreditCostRate(accountEstimatedRate)}</div>
                      <div className="text-[0.62rem] text-muted-foreground">原始 {formatCreditCostRate(accountOriginalRate)}</div>
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs">
                      {account.creditEstimateBlocked ? (
                        <span className="text-destructive" title={account.creditEstimateBlockedReason}>不可用</span>
                      ) : account.accountInfo?.creditRemaining != null ? (
                        formatCredits(account.accountInfo.creditRemaining)
                      ) : (
                        <span className="text-muted-foreground">未查询</span>
                      )}
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs">
                      {account.creditEstimateBlocked ? (
                        <span className="text-destructive" title={account.creditEstimateBlockedReason}>不可估算</span>
                      ) : account.estimatedRemainingCostUsd != null ? (
                        formatUsdFixed2(account.estimatedRemainingCostUsd)
                      ) : (
                        <span className="text-muted-foreground">无法估算</span>
                      )}
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs">{formatPercent(account.metrics.errorRate)}</TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </SectionCard>
  )
}

function ExternalPoolSection({
  external,
  pools,
  range,
  navigate,
}: {
  external: {
    rawCostUsd: number
    shapedCostUsd: number
    upliftedCostUsd: number
    reportedCostUsd: number
    billableCostUsd: number
    profitUsd: number
    costFloorDeltaUsd: number
    costFloorAppliedRequests: number
    pools: ExternalUsagePool[]
  }
  pools: ExternalPool[]
  range: { from: string; to: string }
  navigate: ReturnType<typeof useNavigate>
}) {
  const usageById = new Map(external.pools.map((item) => [item.poolId, item]))
  const configuredIds = new Set(pools.map((pool) => pool.id))
  const rows = [
    ...pools.map((pool) => ({
      pool,
      poolId: pool.id,
      poolName: pool.name,
      priority: pool.priority,
      usage: usageById.get(pool.id),
    })),
    ...external.pools
      .filter((usage) => !configuredIds.has(usage.poolId))
      .map((usage) => ({
        pool: undefined,
        poolId: usage.poolId,
        poolName: usage.poolName || `#${usage.poolId}`,
        priority: Number.MAX_SAFE_INTEGER,
        usage,
      })),
  ]
    .sort((left, right) => (
      (right.usage?.billableCostUsd ?? 0) - (left.usage?.billableCostUsd ?? 0) ||
      (right.usage?.metrics.requests ?? 0) - (left.usage?.metrics.requests ?? 0) ||
      left.priority - right.priority ||
      left.poolId - right.poolId
    ))
  const totalRequests = rows.reduce((sum, item) => sum + (item.usage?.metrics.requests ?? 0), 0)
  const pricedRequests = rows.reduce((sum, item) => sum + (item.usage?.metrics.pricedRequests ?? 0), 0)
  const unpricedRequests = rows.reduce((sum, item) => sum + (item.usage?.metrics.unpricedRequests ?? 0), 0)
  const shapedCost = external.shapedCostUsd ?? external.reportedCostUsd ?? 0
  const upliftedCost = external.upliftedCostUsd ?? external.reportedCostUsd ?? external.billableCostUsd ?? 0
  const billableCost = external.billableCostUsd ?? upliftedCost
  const delta = external.profitUsd
  const deltaTone = billingDeltaTone(delta)
  return (
    <SectionCard>
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard
          title="外部池请求"
          value={formatNumber(totalRequests)}
          desc={`可计价 ${formatNumber(pricedRequests)} / 未计价 ${formatNumber(unpricedRequests)}`}
          icon={<Zap />}
          tone="info"
        />
        <StatCard title="上游原始成本" value={formatUsdFixed2(external.rawCostUsd)} icon={<DollarSign />} tone="warning" />
        <StatCard
          title="展示计费"
          value={formatUsdFixed2(shapedCost)}
          desc="整形后的中间成本口径"
          icon={<DollarSign />}
          tone="primary"
        />
        <StatCard
          title="外部池可计费"
          value={formatUsdFixed2(billableCost)}
          desc={(
            <span>
              补偿后计费 {formatUsdFixed2(upliftedCost)} ·{' '}
              <span className={billingDeltaTextClass(deltaTone)}>
                {delta >= 0 ? '+' : '-'}{formatUsdFixed2(Math.abs(delta))}
              </span>
            </span>
          )}
          icon={<TrendingUp />}
          tone={deltaTone === 'loss' ? 'error' : deltaTone === 'profit' ? 'warning' : 'success'}
        />
      </div>
      <div className="mt-3 rounded-lg bg-muted/25 px-3 py-2 text-xs text-muted-foreground">
        <span className="font-medium text-foreground/75" title="按整形后的展示 usage 计算的中间成本">展示计费</span>
        {' '}是整形后的中间口径；
        <span className="ml-1 font-medium text-foreground/75" title="按最终上报 usage 计算，包含输出补偿和成本底线修复">补偿后计费</span>
        {' '}是最终上报口径，外部池可计费取最终可计费金额。
      </div>
      {rows.length === 0 ? (
        <EmptyState title="暂无外部池配置" className="mt-3 py-8" />
      ) : (
        <div className="mt-4 scrollbar-thin overflow-x-auto">
          <Table className="min-w-[980px] text-xs">
            <TableHeader>
              <TableRow>
                <TableHead>外部池</TableHead>
                <TableHead className="text-right">请求</TableHead>
                <TableHead className="text-right">上游原始成本</TableHead>
                <TableHead className="text-right" title="按整形后的展示 usage 计算的中间成本">展示计费</TableHead>
                <TableHead className="text-right" title="按最终上报 usage 计算，包含输出补偿和成本底线修复">补偿后计费</TableHead>
                <TableHead className="text-right">差额占原始</TableHead>
                <TableHead className="text-right">原始/整形倍率</TableHead>
                <TableHead className="text-right">未计价</TableHead>
                <TableHead className="text-right">兜底</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map(({ pool, poolId, poolName, usage }) => {
                const metrics = usage?.metrics ?? EMPTY_OVERVIEW_METRICS
                const rawCostUsd = usage?.rawCostUsd ?? 0
                const shapedCostUsd = usage?.shapedCostUsd ?? usage?.reportedCostUsd ?? 0
                const upliftedCostUsd = usage?.upliftedCostUsd ?? usage?.reportedCostUsd ?? usage?.billableCostUsd ?? 0
                const poolDelta = usage?.profitUsd ?? (upliftedCostUsd - rawCostUsd)
                const poolTone = billingDeltaTone(poolDelta)
                return (
                  <TableRow key={poolId}>
                    <TableCell>
                      <div className="flex min-w-[180px] flex-wrap items-center gap-1.5">
                        <Button
                          variant="link"
                          size="xs"
                          className="h-auto max-w-[260px] justify-start truncate px-0 text-left text-xs font-semibold"
                          title={`外部池 #${poolId} · 查看 Usage 明细`}
                          onClick={() => usageLink(navigate, {
                            routeKind: 'external_pool',
                            externalPoolId: poolId,
                            since: range.from,
                            until: range.to,
                          })}
                        >
                          #{poolId} {poolName}
                        </Button>
                        {pool && <Badge tone={pool.enabled ? 'success' : 'neutral'} size="xs">{pool.enabled ? '启用' : '停用'}</Badge>}
                        {pool?.autoDisabled && <Badge tone="error" size="xs">自动禁用</Badge>}
                        {metrics.requests === 0 && <Badge tone="secondary" size="xs">无流量</Badge>}
                      </div>
                    </TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(metrics.requests)}>{formatNumber(metrics.requests)}</TableCell>
                    <TableCell className="text-right font-mono">{formatUsdFixed2(rawCostUsd)}</TableCell>
                    <TableCell
                      className="text-right font-mono"
                      title="整形后的中间成本口径"
                    >
                      {formatUsdFixed2(shapedCostUsd)}
                    </TableCell>
                    <TableCell
                      className="text-right font-mono"
                      title="最终上报/补偿后成本口径；当前用于外部池可计费金额"
                    >
                      {formatUsdFixed2(upliftedCostUsd)}
                    </TableCell>
                    <TableCell className={cn('text-right font-mono', billingDeltaTextClass(poolTone))}>
                      <div>{poolDelta >= 0 ? '+' : ''}{formatUsdFixed2(poolDelta)}</div>
                      <div className="text-[0.62rem]" title="当前池差额 ÷ 当前池上游原始成本">{costDeltaPercent(poolDelta, rawCostUsd)}</div>
                    </TableCell>
                    <TableCell className="text-right font-mono" title="上游原始成本 ÷ 整形后展示成本">
                      {costMultiplier(rawCostUsd, shapedCostUsd)}
                    </TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(metrics.unpricedRequests)}>{formatNumber(metrics.unpricedRequests)}</TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(usage?.costFloorAppliedRequests ?? 0)}>{formatNumber(usage?.costFloorAppliedRequests ?? 0)}</TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </SectionCard>
  )
}

function RankPanel({
  rows,
  total,
  errorRank = false,
}: {
  rows: UsageOverviewRankRow[]
  total: number
  errorRank?: boolean
}) {
  return (
    <div className="mt-3">
      {rows.length === 0 ? (
        <EmptyState title="暂无排行数据" className="py-8" />
      ) : (
        <Table className="min-w-[560px]">
          <TableHeader>
            <TableRow>
              <TableHead className="w-10">#</TableHead>
              <TableHead>名称</TableHead>
              <TableHead className="text-right">{errorRank ? '错误数' : '请求数'}</TableHead>
              <TableHead className="text-right">{errorRank ? '占错误' : '占请求'}</TableHead>
              <TableHead className="text-right">估算计费</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, index) => (
              <TableRow key={row.key}>
                <TableCell className="font-mono text-xs text-muted-foreground">{index + 1}</TableCell>
                <TableCell>
                  <div className="max-w-[250px] truncate text-xs font-semibold" title={row.label}>{row.label}</div>
                  {row.label !== row.key && <div className="max-w-[250px] truncate font-mono text-[0.62rem] text-muted-foreground/60">{row.key}</div>}
                </TableCell>
                <TableCell className="text-right font-mono text-xs">{formatCompact(errorRank ? row.errorRequests : row.requests)}</TableCell>
                <TableCell className="text-right font-mono text-xs">{formatPercent(metricsShare(errorRank ? row.errorRequests : row.requests, total))}</TableCell>
                <TableCell className="text-right font-mono text-xs">{formatUsdFixed2(row.estimatedCostUsd)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  )
}

function RankingsSection({
  rankings,
}: {
  rankings: {
    totalRequests: number
    totalLocalRequests: number
    totalErrors: number
    topModels: UsageOverviewRankRow[]
    topAccounts: UsageOverviewRankRow[]
    topKeys: UsageOverviewRankRow[]
    topPaths: UsageOverviewRankRow[]
    topErrors: UsageOverviewRankRow[]
  }
}) {
  const [dimension, setDimension] = useState<RankingDimension>('models')
  const rankViews: Array<{
    key: RankingDimension
    label: string
    rows: UsageOverviewRankRow[]
    total: number
    errorRank?: boolean
  }> = [
    { key: 'models', label: '模型', rows: rankings.topModels, total: rankings.totalRequests },
    { key: 'accounts', label: '账号', rows: rankings.topAccounts, total: rankings.totalLocalRequests },
    { key: 'keys', label: '秘钥', rows: rankings.topKeys, total: rankings.totalRequests },
    { key: 'paths', label: '路径', rows: rankings.topPaths, total: rankings.totalRequests },
    { key: 'errors', label: '错误', rows: rankings.topErrors, total: rankings.totalErrors, errorRank: true },
  ]

  return (
    <SectionCard>
      <Tabs value={dimension} onValueChange={(value) => setDimension(value as RankingDimension)}>
        <TabsList className="w-full justify-start overflow-x-auto">
          {rankViews.map((view) => (
            <TabsTrigger key={view.key} value={view.key}>{view.label}</TabsTrigger>
          ))}
        </TabsList>
        {rankViews.map((view) => (
          <TabsContent key={view.key} value={view.key}>
            <RankPanel
              rows={view.rows}
              total={view.total}
              errorRank={view.errorRank}
            />
          </TabsContent>
        ))}
      </Tabs>
    </SectionCard>
  )
}

export function OverviewPage() {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)

  const tabParam = searchParams.get('tab')
  const activeTab: OverviewTab = tabParam === 'local' || tabParam === 'external' || tabParam === 'rankings'
    ? tabParam
    : 'summary'
  const externalPools = useQuery({
    queryKey: ['external-pools'],
    queryFn: getExternalPools,
    enabled: activeTab === 'external',
    refetchInterval: autoRefresh.refetchInterval || 30000,
  })
  const rangeParam = searchParams.get('range')
  const isPreset = PRESETS.some((item) => item.key === rangeParam)
  const rangeKey = (isPreset ? rangeParam : undefined) as PresetRange | undefined
  const fromParam = searchParams.get('from') ?? ''
  const toParam = searchParams.get('to') ?? ''
  const [customFrom, setCustomFrom] = useState(() => localDateTimeValue(fromParam))
  const [customTo, setCustomTo] = useState(() => localDateTimeValue(toParam))
  const [customError, setCustomError] = useState('')
  const [trendMetric, setTrendMetric] = useState<TrendMetric>('requests')

  const queryParams = useMemo(() => {
    if (rangeKey) return { timezone: TIMEZONE, range: rangeKey }
    if (fromParam && toParam) return { timezone: TIMEZONE, from: fromParam, to: toParam, granularity: searchParams.get('granularity') === 'hour' ? 'hour' as const : 'day' as const }
    return { timezone: TIMEZONE, range: 'today' }
  }, [fromParam, rangeKey, searchParams, toParam])
  const rankingQueryParams = useMemo(() => ({ ...queryParams, topN: 50 as number }), [queryParams])

  const refreshInterval = rangeKey && ['today', 'last24h', 'thisMonth'].includes(rangeKey)
    ? autoRefresh.refetchInterval
    : false
  const summary = useUsageOverviewSummary(queryParams, refreshInterval, activeTab === 'summary' || activeTab === 'local' || activeTab === 'external' || activeTab === 'rankings')
  const series = useUsageOverviewSeries(queryParams, refreshInterval, activeTab === 'summary' && !summary.isError)
  const local = useUsageOverviewLocal(queryParams, refreshInterval, activeTab === 'local' && !summary.isError)
  const external = useUsageOverviewExternal(queryParams, refreshInterval, activeTab === 'external' && !summary.isError)
  const rankings = useUsageOverviewRankings(rankingQueryParams, refreshInterval, activeTab === 'rankings' && !summary.isError)
  const localCredentials = useCredentials({
    enabled: activeTab === 'local',
    refetchInterval: refreshInterval || 30000,
  })
  const localCredentialIds = useMemo(
    () => (localCredentials.data?.credentials ?? []).map((credential) => credential.id),
    [localCredentials.data?.credentials],
  )
  const localCredentialRuntime = useCredentialRuntime(localCredentialIds)
  const localCredentialAccountInfo = useCredentialAccountInfo(localCredentialIds, {
    enabled: activeTab === 'local',
    refetchInterval: refreshInterval || 60000,
  })
  const localAccountRows = useMemo(() => {
    const usageByCredentialId = new Map((local.data?.local.accounts ?? []).map((account) => [account.credentialId, account]))
    const runtimeById = mapById(localCredentialRuntime.data?.items)
    const accountInfoById = mapById(localCredentialAccountInfo.data?.items)
    return (localCredentials.data?.credentials ?? [])
      .map((base) => {
        const credential = mergeCredentialRuntime(base, runtimeById.get(base.id))
        const metrics = usageByCredentialId.get(base.id)?.metrics ?? EMPTY_OVERVIEW_METRICS
        const accountInfo = accountInfoById.get(base.id)
        const creditStatus = credentialCreditStatus(credential)
        const estimatedRemainingCostUsd = creditStatus.available && accountInfo && metrics.kiroMeteringUsage > 0
          ? metrics.estimatedCostUsd / metrics.kiroMeteringUsage * accountInfo.creditRemaining
          : undefined
        return {
          credential,
          credentialId: base.id,
          label: credentialLabel(credential),
          metrics,
          accountInfo,
          creditEstimateBlocked: !creditStatus.available,
          creditEstimateBlockedReason: creditStatus.reason,
          estimatedRemainingCostUsd,
        }
      })
      .sort((left, right) =>
        right.metrics.estimatedCostUsd - left.metrics.estimatedCostUsd ||
        right.metrics.requests - left.metrics.requests ||
        left.credentialId - right.credentialId
      )
  }, [
    local.data?.local.accounts,
    localCredentialAccountInfo.data?.items,
    localCredentialRuntime.data?.items,
    localCredentials.data?.credentials,
  ])

  const setOverviewTab = (tab: OverviewTab) => {
    const next = new URLSearchParams(searchParams)
    if (tab === 'summary') next.delete('tab')
    else next.set('tab', tab)
    setSearchParams(next)
  }

  const applyPreset = (key: PresetRange) => {
    const next = new URLSearchParams(searchParams)
    next.delete('from')
    next.delete('to')
    next.delete('granularity')
    next.delete('backRange')
    next.delete('backFrom')
    next.delete('backTo')
    next.set('range', key)
    setSearchParams(next)
    setCustomError('')
  }

  const applyCustom = () => {
    const from = localDateTimeToIso(customFrom)
    const to = localDateTimeToIso(customTo)
    if (!from || !to || new Date(from) >= new Date(to)) {
      setCustomError('请输入有效的起止时间，且起点必须早于终点。')
      return
    }
    const next = new URLSearchParams(searchParams)
    next.delete('range')
    next.delete('backRange')
    next.delete('backFrom')
    next.delete('backTo')
    next.set('from', from)
    next.set('to', to)
    next.set('granularity', 'day')
    setSearchParams(next)
    setCustomError('')
  }

  const drillDown = (point: UsageOverviewSeriesPoint) => {
    const current = new URLSearchParams(searchParams)
    const start = new Date(point.bucketStart)
    const end = new Date(start)
    if (summary.data?.range.granularity === 'day') end.setUTCDate(end.getUTCDate() + 1)
    else end.setUTCHours(end.getUTCHours() + 1)
    const next = new URLSearchParams(searchParams)
    next.delete('tab')
    next.set('from', start.toISOString())
    next.set('to', end.toISOString())
    next.set('granularity', 'hour')
    if (current.get('range')) next.set('backRange', current.get('range') as string)
    if (current.get('from')) next.set('backFrom', current.get('from') as string)
    if (current.get('to')) next.set('backTo', current.get('to') as string)
    setSearchParams(next)
  }

  const returnFromDrillDown = () => {
    const next = new URLSearchParams(searchParams)
    next.delete('backRange')
    next.delete('backFrom')
    next.delete('backTo')
    const backRange = searchParams.get('backRange')
    const backFrom = searchParams.get('backFrom')
    const backTo = searchParams.get('backTo')
    if (backRange) next.set('range', backRange)
    else if (backFrom && backTo) {
      next.set('from', backFrom)
      next.set('to', backTo)
      next.set('granularity', 'day')
    } else next.set('range', 'today')
    setSearchParams(next)
  }

  const summaryData = summary.data
  const isDrilled = Boolean(searchParams.get('backRange') || searchParams.get('backFrom'))
  const partialErrors = [
    series.error ? `趋势：${extractErrorMessage(series.error)}` : '',
    local.error ? `本地账号池：${extractErrorMessage(local.error)}` : '',
    localCredentials.error ? `本地账号列表：${extractErrorMessage(localCredentials.error)}` : '',
    localCredentialRuntime.error ? `本地账号状态：${extractErrorMessage(localCredentialRuntime.error)}` : '',
    localCredentialAccountInfo.error ? `本地账号余额：${extractErrorMessage(localCredentialAccountInfo.error)}` : '',
    external.error ? `外部池：${extractErrorMessage(external.error)}` : '',
    externalPools.error ? `外部池配置：${extractErrorMessage(externalPools.error)}` : '',
    rankings.error ? `排行：${extractErrorMessage(rankings.error)}` : '',
  ].filter(Boolean)

  const headerActions = (
    <div className="flex w-full flex-wrap items-center justify-between gap-2">
      <div className="flex flex-wrap items-center gap-1.5">
        {PRESETS.map((item) => (
          <Button
            key={item.key}
            size="sm"
            variant={rangeKey === item.key || (!rangeKey && !fromParam && item.key === 'today') ? 'default' : 'outline'}
            onClick={() => applyPreset(item.key)}
          >
            {item.label}
          </Button>
        ))}
        {isDrilled && (
          <Button size="sm" variant="secondary" onClick={returnFromDrillDown}>
            <ArrowLeft className="size-3.5" />返回整体范围
          </Button>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <span>自定义</span>
          <Input type="datetime-local" value={customFrom} onChange={(event) => setCustomFrom(event.target.value)} className="h-8 w-[172px] text-xs" />
          <span>至</span>
          <Input type="datetime-local" value={customTo} onChange={(event) => setCustomTo(event.target.value)} className="h-8 w-[172px] text-xs" />
          <Button size="sm" variant="outline" onClick={applyCustom}>应用</Button>
        </label>
        <Button size="icon-sm" variant="ghost" title="刷新总览" onClick={() => {
          void summary.refetch()
          void series.refetch()
          void local.refetch()
          void localCredentials.refetch()
          if (localCredentialIds.length > 0) void localCredentialRuntime.refetch()
          if (localCredentialIds.length > 0) void localCredentialAccountInfo.refetch()
          void external.refetch()
          void externalPools.refetch()
          void rankings.refetch()
        }}>
          <RefreshCw className={cn('size-4', summary.isFetching && 'animate-spin')} />
        </Button>
      </div>
    </div>
  )

  if (summary.isLoading && !summaryData) {
    return (
      <PageContainer>
        <PageHeader title="总览" actions={headerActions} />
        <LoadingState text="加载整体概览..." />
      </PageContainer>
    )
  }

  if (summary.error || !summaryData) {
    return (
      <PageContainer>
        <PageHeader title="总览" actions={headerActions} />
        <ErrorState title="整体概览加载失败" message={extractErrorMessage(summary.error)} action={<Button size="sm" onClick={() => void summary.refetch()}>重试</Button>} />
      </PageContainer>
    )
  }

  return (
    <PageContainer>
      <PageHeader title="总览" actions={headerActions} />

      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <span className="font-semibold text-foreground">{rangeLabel(summaryData.range)}</span>
        <span>·</span>
        <span>数据截至 {formatDate(summaryData.range.dataThrough)}</span>
        <span>·</span>
        <span>范围已按整点对齐：{formatDate(summaryData.range.from)} - {formatDate(summaryData.range.to)}</span>
        {summaryData.range.granularity === 'day' && <Badge tone="neutral">按天聚合</Badge>}
        {refreshInterval && <Badge tone="success">每 {autoRefresh.intervalSeconds} 秒刷新</Badge>}
      </div>

      {customError && <Callout tone="warning">{customError}</Callout>}
      {partialErrors.length > 0 && <Callout tone="warning">部分分区加载失败：{partialErrors.join('；')}</Callout>}

      <Tabs value={activeTab} onValueChange={(value) => setOverviewTab(value as OverviewTab)}>
        <TabsList className="w-full justify-start overflow-x-auto">
          <TabsTrigger value="summary">汇总</TabsTrigger>
          <TabsTrigger value="local">本地账号</TabsTrigger>
          <TabsTrigger value="external">外部池</TabsTrigger>
          <TabsTrigger value="rankings">排行</TabsTrigger>
        </TabsList>

        <TabsContent value="summary">
          <div className="space-y-3">
            <OverallSummarySection metrics={summaryData.totals.all} />
            <TrendPanel
              points={series.data?.series ?? []}
              metric={trendMetric}
              onMetricChange={setTrendMetric}
              onDrillDown={drillDown}
            />
          </div>
        </TabsContent>

        <TabsContent value="local">
          {local.isLoading && !local.data ? (
            <LoadingState text="加载本地账号池..." />
          ) : (
            <LocalPoolSection
              metrics={summaryData.totals.local}
              accountRows={localAccountRows}
              accountsLoading={localCredentials.isLoading || (localCredentialIds.length > 0 && localCredentialRuntime.isLoading)}
              range={summaryData.range}
              navigate={navigate}
            />
          )}
        </TabsContent>

        <TabsContent value="external">
          {external.isLoading && !external.data ? (
            <LoadingState text="加载外部池..." />
          ) : (
            <ExternalPoolSection
              external={external.data?.external ?? {
                rawCostUsd: 0,
                shapedCostUsd: 0,
                upliftedCostUsd: 0,
                reportedCostUsd: 0,
                billableCostUsd: 0,
                profitUsd: 0,
                costFloorDeltaUsd: 0,
                costFloorAppliedRequests: 0,
                pools: [],
              }}
              pools={externalPools.data?.pools ?? []}
              range={summaryData.range}
              navigate={navigate}
            />
          )}
        </TabsContent>

        <TabsContent value="rankings">
          {rankings.isLoading && !rankings.data ? (
            <LoadingState text="加载排行..." />
          ) : (
            <RankingsSection
              rankings={{
                totalRequests: rankings.data?.totalRequests ?? summaryData.totals.all.requests,
                totalLocalRequests: rankings.data?.totalLocalRequests ?? summaryData.totals.local.requests,
                totalErrors: rankings.data?.totalErrors ?? summaryData.totals.all.errorRequests,
                topModels: rankings.data?.topModels ?? [],
                topAccounts: rankings.data?.topAccounts ?? [],
                topKeys: rankings.data?.topKeys ?? [],
                topPaths: rankings.data?.topPaths ?? [],
                topErrors: rankings.data?.topErrors ?? [],
              }}
            />
          )}
        </TabsContent>
      </Tabs>

    </PageContainer>
  )
}
