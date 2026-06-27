import { useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import {
  Activity,
  CheckCircle2,
  Clock3,
  Database,
  DollarSign,
  RefreshCw,
  ShieldAlert,
  TrendingUp,
  Users,
  Zap,
} from 'lucide-react'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useUsageDashboard } from '@/hooks/use-usage'
import { useCredentialSummary } from '@/hooks/use-credentials'
import { formatCompact, formatDate, formatNumber, formatPercent, formatUsd } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import { billingDeltaTone, billingDeltaTextClass } from '../usage/usage-helpers'
import type {
  UsageBreakdownItem,
  UsageDashboardWindow,
  UsageExternalPoolBillingByPool,
  UsageExternalPoolBillingSummary,
  UsageSeriesPoint,
  UsageTopAggregate,
} from '@/types/api'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  EmptyState,
  LoadingState,
  ErrorState,
  Callout,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Switch,
  Input,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'
import {
  TrendAreaChart,
  TrendBarChart,
  Sparkline,
  ProgressRing,
  CHART_COLORS,
} from '@/components/charts'

// ─── 常量 ─────────────────────────────────────────────────────────────────────

const OVERVIEW_TIMEZONE = 'Asia/Shanghai'
const OVERVIEW_AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:overview'

const EMPTY_EXTERNAL_POOL_BILLING: UsageExternalPoolBillingSummary = {
  requests: 0,
  pricedRequests: 0,
  unpricedRequests: 0,
  costFloorAppliedRequests: 0,
  rawCostUsd: 0,
  shapedCostUsd: 0,
  upliftedCostUsd: 0,
  profitUsd: 0,
  reportedCostUsd: 0,
  billableCostUsd: 0,
  costFloorDeltaUsd: 0,
}

type RankDimension = 'models' | 'credentials' | 'endpoints' | 'errors'

const rankDimensions: Array<{ key: RankDimension; label: string }> = [
  { key: 'models', label: '模型' },
  { key: 'credentials', label: '账号' },
  { key: 'endpoints', label: '入口' },
  { key: 'errors', label: '错误' },
]

const EMPTY_TOP = {
  models: [] as UsageTopAggregate[],
  credentials: [] as UsageTopAggregate[],
  endpoints: [] as UsageTopAggregate[],
  errors: [] as UsageTopAggregate[],
}

// ─── 工具函数 ──────────────────────────────────────────────────────────────────

function activeWindow(windows: UsageDashboardWindow[], key: string): UsageDashboardWindow | undefined {
  return windows.find((w) => w.key === key) ?? windows[0]
}

function seriesPointToChartRow(p: UsageSeriesPoint): Record<string, number | string> {
  return {
    label: p.label,
    requests: p.requests,
    errors: p.errorRequests,
    cost: p.totalEstimatedCostUsd,
    inputTokens: p.totalInputTokens,
    outputTokens: p.totalOutputTokens,
  }
}

function formatDuration(ms: number): string {
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)}m`
  if (ms >= 1_000) return `${(ms / 1_000).toFixed(1)}s`
  return `${Math.round(ms)}ms`
}

// ─── 子组件：趋势图区 ──────────────────────────────────────────────────────────

function TrendSection({
  hourly,
  daily,
}: {
  hourly: UsageSeriesPoint[]
  daily: UsageSeriesPoint[]
}) {
  const hourlyData = hourly.map(seriesPointToChartRow)
  const dailyData = daily.map(seriesPointToChartRow)
  const hourlyErrors = hourly.reduce((s, p) => s + p.errorRequests, 0)
  const dailyErrors = daily.reduce((s, p) => s + p.errorRequests, 0)

  return (
    <div className="grid gap-3 xl:grid-cols-2">
      <SectionCard
        title="最近 24 小时"
        description="按小时聚合的请求量与错误"
        actions={
          hourlyErrors > 0
            ? <Badge tone="error" title={formatNumber(hourlyErrors)}>错误 {formatCompact(hourlyErrors)}</Badge>
            : <Badge tone="success">无错误</Badge>
        }
      >
        {hourlyData.length === 0 ? (
          <EmptyState title="暂无数据" className="py-8" />
        ) : (
          <TrendAreaChart
            data={hourlyData}
            xKey="label"
            series={[
              { key: 'requests', name: '请求', color: CHART_COLORS[0] },
              { key: 'errors', name: '错误', color: CHART_COLORS[4] },
            ]}
            height={200}
            valueFormatter={(v) => formatNumber(Number(v))}
          />
        )}
      </SectionCard>
      <SectionCard
        title="最近 7 天"
        description="按天聚合的请求量与错误"
        actions={
          dailyErrors > 0
            ? <Badge tone="error" title={formatNumber(dailyErrors)}>错误 {formatCompact(dailyErrors)}</Badge>
            : <Badge tone="success">无错误</Badge>
        }
      >
        {dailyData.length === 0 ? (
          <EmptyState title="暂无数据" className="py-8" />
        ) : (
          <TrendBarChart
            data={dailyData}
            xKey="label"
            series={[
              { key: 'requests', name: '请求', color: CHART_COLORS[0] },
              { key: 'errors', name: '错误', color: CHART_COLORS[4] },
            ]}
            height={200}
            valueFormatter={(v) => formatNumber(Number(v))}
          />
        )}
      </SectionCard>
    </div>
  )
}

// ─── 子组件：账号池状态 ────────────────────────────────────────────────────────

function CredentialPoolPanel() {
  const { data, isLoading } = useCredentialSummary()

  if (isLoading || !data) {
    return (
      <SectionCard title="账号池" description="可用 / 禁用 / 并发">
        <LoadingState text="加载账号池..." className="py-6" />
      </SectionCard>
    )
  }

  const total = data.total ?? 0
  const available = data.available ?? 0
  const disabled = data.disabled ?? 0
  const cooling = total - available - disabled
  const concurrency = data.globalInFlightRequests ?? 0
  const maxConcurrency = data.globalMaxConcurrentRequests ?? 0
  const queued = data.queuedRequests ?? 0
  const availRatio = total > 0 ? available / total : 0
  const concRatio = maxConcurrency > 0 ? concurrency / maxConcurrency : 0

  return (
    <SectionCard
      title="账号池"
      description="实时可用状态与并发占用"
      icon={<Users />}
      actions={
        available === 0
          ? <Badge tone="error">无可用账号</Badge>
          : availRatio < 0.3
            ? <Badge tone="warning">可用偏低</Badge>
            : <Badge tone="success">正常</Badge>
      }
    >
      <div className="flex flex-wrap items-center gap-6">
        <ProgressRing
          value={availRatio * 100}
          size={72}
          strokeWidth={7}
          color="hsl(var(--success))"
          trackColor="hsl(var(--muted))"
          label={<span className="text-[0.68rem] font-bold">{Math.round(availRatio * 100)}%</span>}
        />
        <div className="grid flex-1 grid-cols-2 gap-x-6 gap-y-2 text-xs min-w-[180px]">
          <PoolStatRow label="可用" value={available} tone="success" />
          <PoolStatRow label="禁用" value={disabled} tone="error" />
          <PoolStatRow label="冷却" value={Math.max(0, cooling)} tone="warning" />
          <PoolStatRow label="合计" value={total} tone="default" />
        </div>
        <div className="border-l border-border pl-6 grid gap-2 text-xs min-w-[160px]">
          <div className="flex items-center justify-between gap-4">
            <span className="text-muted-foreground">并发占用</span>
            <span className="tabular-nums font-semibold text-foreground">
              {formatNumber(concurrency)}{maxConcurrency > 0 ? ` / ${formatNumber(maxConcurrency)}` : ''}
            </span>
          </div>
          {maxConcurrency > 0 && (
            <div className="h-1.5 overflow-hidden rounded-full bg-muted">
              <div
                className={cn('h-full rounded-full transition-all', concRatio > 0.8 ? 'bg-warning' : 'bg-primary')}
                style={{ width: `${Math.min(100, concRatio * 100)}%` }}
              />
            </div>
          )}
          {queued > 0 && (
            <div className="flex items-center justify-between gap-4">
              <span className="text-warning">排队请求</span>
              <span className="tabular-nums font-semibold text-warning">{formatNumber(queued)}</span>
            </div>
          )}
        </div>
      </div>
    </SectionCard>
  )
}

function PoolStatRow({
  label,
  value,
  tone,
}: {
  label: string
  value: number
  tone: 'success' | 'error' | 'warning' | 'default'
}) {
  const cls = {
    success: 'text-success',
    error: 'text-destructive',
    warning: 'text-warning',
    default: 'text-foreground',
  }[tone]
  return (
    <div className="flex items-center justify-between gap-2">
      <span className="text-muted-foreground">{label}</span>
      <span className={cn('tabular-nums font-semibold', cls)}>{formatNumber(value)}</span>
    </div>
  )
}

// ─── 子组件：异常摘要 ──────────────────────────────────────────────────────────

function ErrorSummaryPanel({
  totalErrors,
  errorRate,
  items,
}: {
  totalErrors: number
  errorRate: number
  items: UsageTopAggregate[]
}) {
  const visible = items.filter((i) => i.requests > 0).slice(0, 5)
  const isHigh = errorRate >= 0.2
  const hasAny = totalErrors > 0

  return (
    <SectionCard
      title="异常摘要"
      description="需要排障的错误聚合，完整明细到用量页筛选"
      icon={<ShieldAlert />}
      actions={
        hasAny
          ? <Badge tone={isHigh ? 'error' : 'warning'} title={formatNumber(totalErrors)}>{formatCompact(totalErrors)} 错误</Badge>
          : <Badge tone="success">正常</Badge>
      }
    >
      {hasAny && (
        <Callout tone={isHigh ? 'error' : 'warning'} className="mb-3">
          {isHigh
            ? `错误率 ${formatPercent(errorRate)} — 已超过 20%，建议立即排查。`
            : `当前窗口存在 ${formatNumber(totalErrors)} 个错误请求（${formatPercent(errorRate)}），请关注。`}
        </Callout>
      )}
      <div className="space-y-2">
        {visible.length === 0 ? (
          <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
            <CheckCircle2 className="size-4 shrink-0 text-success" />
            当前窗口无错误聚合
          </div>
        ) : (
          visible.map((item, idx) => (
            <div
              key={`${item.key}-${idx}`}
              className="relative overflow-hidden rounded-lg border border-border bg-card px-3 py-2.5"
            >
              <div className="absolute inset-y-3 left-0 w-0.5 rounded-r bg-destructive/70" />
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0 pl-1.5">
                  <div className="truncate text-xs font-semibold text-destructive" title={item.label ?? item.key}>
                    {item.label ?? item.key}
                  </div>
                  {item.label && (
                    <div className="truncate font-mono text-[0.62rem] text-muted-foreground/60">{item.key}</div>
                  )}
                </div>
                <Badge tone="error" title={formatNumber(item.requests)}>{formatCompact(item.requests)}</Badge>
              </div>
              <div className="mt-1.5 grid grid-cols-3 gap-1 pl-1.5 text-[0.62rem] text-muted-foreground/60">
                <span title={formatNumber(item.totalInputTokens)}>输入 {formatCompact(item.totalInputTokens)}</span>
                <span title={formatNumber(item.totalOutputTokens)}>输出 {formatCompact(item.totalOutputTokens)}</span>
                <span className="text-right">{formatUsd(item.totalEstimatedCostUsd)}</span>
              </div>
            </div>
          ))
        )}
      </div>
    </SectionCard>
  )
}

// ─── 子组件：运行信号 ──────────────────────────────────────────────────────────

function SignalRow({
  label,
  value,
  ratio,
  barColor = 'bg-primary/65',
  title,
}: {
  label: string
  value: ReactNode
  ratio?: number
  barColor?: string
  title?: string
}) {
  const width = Number.isFinite(ratio ?? NaN)
    ? Math.min(100, Math.max(0, (ratio as number) * 100))
    : 0
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="truncate font-medium text-foreground/75" title={title}>{label}{title && <span className="ml-1 cursor-help text-muted-foreground/50">ⓘ</span>}</span>
        <span className="shrink-0 font-mono text-xs text-muted-foreground">{value}</span>
      </div>
      {ratio !== undefined && (
        <div className="h-1.5 overflow-hidden rounded-full bg-muted">
          <div className={cn('h-full rounded-full', barColor)} style={{ width: `${width}%` }} />
        </div>
      )}
    </div>
  )
}

function RunSignalsPanel({
  pricedRatio,
  streamRatio,
  cacheReadRatio,
  stickyBound,
  stickyFallback,
  simulated,
  upstreamMeta,
}: {
  pricedRatio: number
  streamRatio: number
  cacheReadRatio: number
  stickyBound: number
  stickyFallback: number
  simulated: number
  upstreamMeta: number
}) {
  return (
    <SectionCard title="运行信号" description="调度与缓存指标，判断配置是否异常" icon={<Activity />}>
      <div className="grid gap-3 sm:grid-cols-2">
        <SignalRow
          label="计价覆盖"
          value={formatPercent(pricedRatio)}
          ratio={pricedRatio}
          barColor={pricedRatio < 1 ? 'bg-warning/80' : 'bg-success/80'}
        />
        <SignalRow
          label="流式占比"
          value={formatPercent(streamRatio)}
          ratio={streamRatio}
          barColor="bg-info/70"
        />
        <SignalRow
          label="缓存读取率"
          value={formatPercent(cacheReadRatio)}
          ratio={cacheReadRatio}
          barColor="bg-success/80"
        />
        <SignalRow
          label="Sticky 回退"
          value={<span title={`${formatNumber(stickyFallback)} / ${formatNumber(stickyBound)}`}>{formatCompact(stickyFallback)} / {formatCompact(stickyBound)}</span>}
          ratio={stickyBound > 0 ? stickyFallback / stickyBound : 0}
          barColor={stickyFallback > 0 ? 'bg-warning/80' : 'bg-muted-foreground/30'}
        />
        <SignalRow
          label="模拟展示"
          value={<span title={formatNumber(simulated)}>{formatCompact(simulated)}</span>}
          title="该窗口内用量由本地模拟计算展示（非服务实际返回），通常出现在流式响应或无用量元数据时"
        />
        <SignalRow
          label="服务返回用量"
          value={<span title={formatNumber(upstreamMeta)}>{formatCompact(upstreamMeta)}</span>}
          ratio={upstreamMeta > 0 ? upstreamMeta / (simulated + upstreamMeta) : 0}
          barColor="bg-primary/65"
          title="该窗口内用量直接由上游服务返回（upstream_metadata），精度最高"
        />
      </div>
    </SectionCard>
  )
}

// ─── 子组件：维度排行 ──────────────────────────────────────────────────────────

function DimensionRankPanel({
  top,
  activeKey,
  onActiveKeyChange,
}: {
  top: {
    models: UsageTopAggregate[]
    credentials: UsageTopAggregate[]
    endpoints: UsageTopAggregate[]
    errors: UsageTopAggregate[]
  }
  activeKey: RankDimension
  onActiveKeyChange: (k: RankDimension) => void
}) {
  const items = top[activeKey] ?? []
  const totalReqs = items.reduce((s, i) => s + i.requests, 0)

  return (
    <SectionCard
      title="维度排行"
      description="按请求量聚合的 Top 维度，切换查看"
      icon={<TrendingUp />}
      actions={
        <div className="inline-flex overflow-hidden rounded-lg border border-border">
          {rankDimensions.map((dim) => (
            <Button
              key={dim.key}
              variant={dim.key === activeKey ? 'default' : 'ghost'}
              size="xs"
              className="rounded-none"
              onClick={() => onActiveKeyChange(dim.key)}
            >
              {dim.label}
            </Button>
          ))}
        </div>
      }
      noPadding
    >
      {items.length === 0 ? (
        <div className="px-4 py-3 text-xs text-muted-foreground/60">暂无排行数据</div>
      ) : (
        <div className="scrollbar-thin overflow-x-auto">
          <Table className="min-w-[560px]">
            <TableHeader>
              <TableRow>
                <TableHead className="w-8">#</TableHead>
                <TableHead>名称</TableHead>
                <TableHead className="text-right">请求</TableHead>
                <TableHead className="text-right">错误</TableHead>
                <TableHead className="text-right">输入 Token</TableHead>
                <TableHead className="text-right">输出 Token</TableHead>
                <TableHead className="text-right">估算费用</TableHead>
                <TableHead className="w-28">占比</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.slice(0, 10).map((item, idx) => (
                <TableRow key={`${item.key}-${idx}`}>
                  <TableCell className="text-muted-foreground/60 font-mono text-xs">{idx + 1}</TableCell>
                  <TableCell>
                    <div className="max-w-[200px] truncate text-xs font-semibold" title={item.label ?? item.key}>
                      {item.label ?? item.key}
                    </div>
                    {item.label && (
                      <div className="font-mono text-[0.62rem] text-muted-foreground/60 truncate max-w-[200px]">
                        {item.key}
                      </div>
                    )}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs" title={formatNumber(item.requests)}>{formatCompact(item.requests)}</TableCell>
                  <TableCell className="text-right font-mono text-xs">
                    {item.errorRequests > 0
                      ? <span className="text-destructive" title={formatNumber(item.errorRequests)}>{formatCompact(item.errorRequests)}</span>
                      : <span className="text-muted-foreground/40">—</span>}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs" title={formatNumber(item.totalInputTokens)}>{formatCompact(item.totalInputTokens)}</TableCell>
                  <TableCell className="text-right font-mono text-xs" title={formatNumber(item.totalOutputTokens)}>{formatCompact(item.totalOutputTokens)}</TableCell>
                  <TableCell className="text-right font-mono text-xs">{formatUsd(item.totalEstimatedCostUsd)}</TableCell>
                  <TableCell>
                    <div className="h-1.5 overflow-hidden rounded-full bg-muted">
                      <div
                        className="h-full rounded-full bg-primary/65"
                        style={{ width: `${totalReqs > 0 ? Math.min(100, (item.requests / totalReqs) * 100) : 0}%` }}
                      />
                    </div>
                    <span className="text-[0.62rem] text-muted-foreground/60 tabular-nums">
                      {formatPercent(totalReqs > 0 ? item.requests / totalReqs : 0)}
                    </span>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </SectionCard>
  )
}

// ─── 子组件：外部账号计费拆分 ──────────────────────────────────────────────────

function ExternalPoolBillingPanel({
  billing,
  billingByPool,
}: {
  billing: UsageExternalPoolBillingSummary
  billingByPool: UsageExternalPoolBillingByPool[]
}) {
  const shapedCost = billing.shapedCostUsd ?? billing.reportedCostUsd ?? 0
  const upliftedCost = billing.upliftedCostUsd ?? billing.reportedCostUsd ?? billing.billableCostUsd ?? 0
  const profit = billing.profitUsd ?? (upliftedCost - (billing.rawCostUsd || 0))
  const profitRatio = billing.rawCostUsd > 0 ? profit / billing.rawCostUsd : 0
  const deltaTone = billingDeltaTone(profit)
  const hasLoss = deltaTone === 'loss'
  const hasProfit = deltaTone === 'profit'
  const visiblePools = billingByPool.filter((pool) => pool.requests > 0).slice(0, 20)

  return (
    <SectionCard
      title="外部账号计费拆分"
      description="展示外部账号的成本、计费金额和差额，便于判断外部账号是否划算"
      icon={<DollarSign />}
      actions={
        <span className={cn(
          'rounded border px-2 py-0.5 text-[0.68rem] font-semibold',
          hasLoss
            ? 'border-destructive/25 bg-card text-destructive'
            : hasProfit
              ? 'border-warning/25 bg-card text-warning'
              : 'border-border bg-card text-muted-foreground/55'
        )}>
          {hasLoss ? `亏损 ${formatUsd(Math.abs(profit))}` : hasProfit ? `盈利 ${formatUsd(profit)}` : '持平'}
        </span>
      }
    >
      <div className="space-y-4">
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          <div className="rounded-lg border border-border bg-muted/40 p-3">
            <div className="text-xs text-muted-foreground">外部账号请求</div>
            <div className="mt-1 text-lg font-semibold" title={formatNumber(billing.requests)}>{formatCompact(billing.requests)}</div>
            <div className="mt-1 text-xs text-muted-foreground/60">
              可计价 <span title={formatNumber(billing.pricedRequests)}>{formatCompact(billing.pricedRequests)}</span> / 未计价 <span title={formatNumber(billing.unpricedRequests)}>{formatCompact(billing.unpricedRequests)}</span>
            </div>
          </div>
          <div className="rounded-lg border border-border bg-muted/40 p-3">
            <div className="text-xs text-muted-foreground">原始成本</div>
            <div className="mt-1 text-lg font-semibold">{formatUsd(billing.rawCostUsd)}</div>
            <div className="mt-1 text-xs text-muted-foreground/60">按外部账号实际消耗估算</div>
          </div>
          <div className="rounded-lg border border-border bg-muted/40 p-3">
            <div className="text-xs text-muted-foreground">展示计费</div>
            <div className="mt-1 text-lg font-semibold">{formatUsd(shapedCost)}</div>
            <div className="mt-1 text-xs text-muted-foreground/60">按当前展示规则计算</div>
          </div>
          <div className="rounded-lg border border-border bg-muted/40 p-3">
            <div className="text-xs text-muted-foreground">补偿后计费</div>
            <div className="mt-1 text-lg font-semibold">{formatUsd(upliftedCost)}</div>
            <div className={cn('mt-1 text-xs', billingDeltaTextClass(deltaTone))}>
              盈利 = 放大后 - 原始：{profit >= 0 ? '+' : ''}{formatUsd(profit)}
            </div>
          </div>
        </div>

        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-2 text-xs">
            <span className="truncate font-medium text-foreground/75">盈利占原始成本</span>
            <span className="shrink-0 font-mono text-muted-foreground">
              {profit >= 0 ? '+' : ''}{formatUsd(profit)} · {formatPercent(profitRatio)}
            </span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-muted">
            <div
              className={cn('h-full rounded-full', hasLoss ? 'bg-warning/80' : 'bg-success/80')}
              style={{ width: `${Math.min(100, Math.abs(profitRatio) * 100)}%` }}
            />
          </div>
        </div>

        <div className="border-t border-border pt-3">
          <div className="mb-2 flex items-center justify-between gap-2">
            <div className="text-xs font-semibold text-foreground/70">外部账号成本与盈亏</div>
            <div className="text-[0.68rem] text-muted-foreground/45">按当前时间窗口聚合</div>
          </div>
          {visiblePools.length === 0 ? (
            <div className="rounded-lg border border-dashed border-border p-3 text-sm text-muted-foreground/60">
              当前窗口没有外部账号计费样本。
            </div>
          ) : (
            <div className="scrollbar-thin overflow-x-auto rounded-lg border border-border">
              <table className="w-full min-w-[640px] text-xs">
                <thead>
                  <tr className="border-b border-border bg-muted/40 text-muted-foreground">
                    <th className="px-3 py-2 text-left font-medium">外部账号</th>
                    <th className="px-3 py-2 text-right font-medium">请求</th>
                    <th className="px-3 py-2 text-right font-medium">原始成本</th>
                    <th className="px-3 py-2 text-right font-medium">展示计费</th>
                    <th className="px-3 py-2 text-right font-medium">补偿后</th>
                    <th className="px-3 py-2 text-right font-medium">盈亏</th>
                    <th className="px-3 py-2 text-right font-medium">未计价</th>
                    <th className="px-3 py-2 text-right font-medium">兜底</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border">
                  {visiblePools.map((pool) => {
                    const poolProfit = pool.profitUsd ?? ((pool.upliftedCostUsd ?? pool.reportedCostUsd ?? 0) - pool.rawCostUsd)
                    const poolTone = billingDeltaTone(poolProfit)
                    return (
                      <tr key={pool.poolId} className="bg-card hover:bg-muted/30 transition-colors">
                        <td className="px-3 py-2">
                          <div className="max-w-[200px] truncate font-medium" title={pool.poolName}>{pool.poolName}</div>
                          <div className="font-mono text-[0.62rem] text-muted-foreground/45">#{pool.poolId}</div>
                        </td>
                        <td className="px-3 py-2 text-right font-mono" title={formatNumber(pool.requests)}>{formatCompact(pool.requests)}</td>
                        <td className="px-3 py-2 text-right font-mono">{formatUsd(pool.rawCostUsd)}</td>
                        <td className="px-3 py-2 text-right font-mono">{formatUsd(pool.shapedCostUsd ?? pool.reportedCostUsd)}</td>
                        <td className="px-3 py-2 text-right font-mono">{formatUsd(pool.upliftedCostUsd ?? pool.reportedCostUsd)}</td>
                        <td className={cn('px-3 py-2 text-right font-mono', billingDeltaTextClass(poolTone))}>
                          {poolProfit >= 0 ? '+' : ''}{formatUsd(poolProfit)}
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

// ─── 子组件：占比分解面板（Tab 合并版） ────────────────────────────────────────

const BREAKDOWN_TONES: Record<string, string> = {
  success: 'bg-success/80',
  timeout: 'bg-warning/80',
  upstream_timeout: 'bg-warning/80',
  client_error: 'bg-destructive/70',
  stream_error: 'bg-destructive/70',
  error: 'bg-destructive/70',
  upstream_metadata: 'bg-primary/65',
  local_prompt_cache: 'bg-success/70',
  context_estimate: 'bg-info/70',
  request_estimate: 'bg-info/60',
}

function breakdownBarColor(key: string): string {
  return BREAKDOWN_TONES[key] ?? 'bg-muted-foreground/40'
}

type BreakdownTab = 'status' | 'source'

function BreakdownTabPanel({
  statusItems,
  sourceItems,
}: {
  statusItems: UsageBreakdownItem[]
  sourceItems: UsageBreakdownItem[]
}) {
  const [activeTab, setActiveTab] = useState<BreakdownTab>('status')
  const items = activeTab === 'status' ? statusItems : sourceItems
  const emptyText = activeTab === 'status' ? '暂无状态样本。' : '暂无来源样本。'

  return (
    <SectionCard
      title={activeTab === 'status' ? '状态分布' : '用量来源'}
      description={activeTab === 'status' ? '成功、超时、客户端错误等整体占比' : '用量来自服务返回、缓存展示或系统补充的占比'}
      actions={
        <div className="inline-flex overflow-hidden rounded-lg border border-border">
          <Button
            variant={activeTab === 'status' ? 'default' : 'ghost'}
            size="xs"
            className="rounded-none"
            onClick={() => setActiveTab('status')}
          >
            状态分布
          </Button>
          <Button
            variant={activeTab === 'source' ? 'default' : 'ghost'}
            size="xs"
            className="rounded-none"
            onClick={() => setActiveTab('source')}
          >
            用量来源
          </Button>
        </div>
      }
    >
      <div className="space-y-3">
        {items.length === 0 ? (
          <div className="rounded-lg border border-dashed border-border px-3 py-3 text-sm text-muted-foreground/60">
            {emptyText}
          </div>
        ) : (
          items.slice(0, 6).map((item) => {
            const width = Number.isFinite(item.ratio) ? Math.min(100, Math.max(0, item.ratio * 100)) : 0
            const barColor = breakdownBarColor(item.key)
            return (
              <div key={item.key} className="space-y-1.5">
                <div className="flex items-center justify-between gap-2 text-xs">
                  <span className="truncate font-medium text-foreground/75">{item.label}</span>
                  <span className="shrink-0 font-mono text-muted-foreground" title={formatNumber(item.requests)}>
                    {formatCompact(item.requests)} · {formatPercent(item.ratio)}
                  </span>
                </div>
                <div className="h-1.5 overflow-hidden rounded-full bg-muted">
                  <div className={cn('h-full rounded-full', barColor)} style={{ width: `${width}%` }} />
                </div>
              </div>
            )
          })
        )}
      </div>
    </SectionCard>
  )
}

// ─── 主页 ──────────────────────────────────────────────────────────────────────

export function OverviewPage() {
  const autoRefresh = useAutoRefreshPreference(OVERVIEW_AUTO_REFRESH_KEY, 30)
  const dashboard = useUsageDashboard(OVERVIEW_TIMEZONE, autoRefresh.refetchInterval)
  const [selectedWindowKey, setSelectedWindowKey] = useState('today')
  const [rankDimension, setRankDimension] = useState<RankDimension>('credentials')

  const data = dashboard.data
  const selectedWindow = useMemo(
    () => activeWindow(data?.windows ?? [], selectedWindowKey),
    [data?.windows, selectedWindowKey]
  )

  // 加载态
  if (dashboard.isLoading) {
    return (
      <PageContainer>
        <PageHeader title="总览" subtitle="实时健康、关键指标与异常" />
        <LoadingState text="正在加载总览数据..." />
      </PageContainer>
    )
  }

  // 错误态
  if (dashboard.error) {
    return (
      <PageContainer>
        <PageHeader title="总览" subtitle="实时健康、关键指标与异常" />
        <ErrorState title="总览加载失败" message={extractErrorMessage(dashboard.error)} />
      </PageContainer>
    )
  }

  // 空态
  if (!data || !selectedWindow) {
    return (
      <PageContainer>
        <PageHeader title="总览" subtitle="实时健康、关键指标与异常" />
        <EmptyState title="暂无总览数据" description="当前还没有可聚合的请求记录。" />
      </PageContainer>
    )
  }

  const summary = selectedWindow.summary
  const top = data.top ?? EMPTY_TOP
  const series = data.series ?? { hourly24h: [], daily7d: [] }

  const pricedRatio = summary.totalRequests > 0 ? summary.pricedRequests / summary.totalRequests : 0
  const streamRatio = summary.totalRequests > 0 ? summary.streamRequests / summary.totalRequests : 0
  const totalTokens = summary.totalInputTokens + summary.totalOutputTokens
  const latencyTone: 'warning' | 'info' = summary.p95DurationMs >= 60_000 ? 'warning' : 'info'

  // 为 StatCard 准备 Sparkline 数据（用 hourly24h 序列）
  const sparkData = (series.hourly24h ?? []).map(seriesPointToChartRow)

  const headerActions = (
    <div className="flex flex-wrap items-center gap-2">
      {/* 时间窗口 */}
      <div className="inline-flex overflow-hidden rounded-lg border border-border">
        {data.windows.map((w) => (
          <Button
            key={w.key}
            variant={w.key === selectedWindow.key ? 'default' : 'ghost'}
            size="sm"
            className="rounded-none"
            onClick={() => setSelectedWindowKey(w.key)}
          >
            {w.label}
          </Button>
        ))}
      </div>
      {/* 自动刷新 */}
      <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer select-none">
        <Switch checked={autoRefresh.enabled} onCheckedChange={autoRefresh.setEnabled} />
        自动刷新
      </label>
      <div className="flex items-center gap-1">
        <Input
          type="number"
          min={5}
          max={3600}
          className="h-8 w-16 text-xs"
          value={autoRefresh.intervalSeconds}
          disabled={!autoRefresh.enabled}
          onChange={(e) => autoRefresh.setIntervalSeconds(Number(e.target.value))}
          onBlur={(e) => {
            const v = Math.max(5, Math.min(3600, Number(e.target.value) || 30))
            autoRefresh.setIntervalSeconds(v)
          }}
        />
        <span className="text-xs text-muted-foreground">秒</span>
      </div>
      {dashboard.isFetching && (
        <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />
      )}
    </div>
  )

  return (
    <PageContainer>
      <PageHeader title="总览" subtitle="实时健康、关键指标与异常" actions={headerActions} />

      {/* 1. 关键指标卡 */}
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-6">
        {/* 请求量 + Sparkline */}
        <div className="relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl border border-border bg-card p-4 shadow-sm transition-colors hover:border-border-strong">
          <span className="absolute left-0 top-4 h-8 w-1 rounded-r-full bg-primary" />
          <div className="flex items-start justify-between gap-2 pl-2.5">
            <div className="min-w-0 flex-1">
              <div className="text-[0.72rem] font-semibold text-muted-foreground">请求量</div>
              <div className="mt-1 text-2xl font-semibold tracking-tight tabular-nums text-primary" title={formatNumber(summary.totalRequests)}>
                {formatCompact(summary.totalRequests)}
              </div>
            </div>
            <div className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-border bg-muted text-primary [&_svg]:size-4">
              <Activity />
            </div>
          </div>
          <div className="mt-1 pl-2.5">
            {sparkData.length > 0 && (
              <Sparkline data={sparkData} dataKey="requests" color={CHART_COLORS[0]} height={28} />
            )}
            <div className="mt-1 truncate text-[0.72rem] text-muted-foreground">
              成功 <span title={formatNumber(summary.successRequests)}>{formatCompact(summary.successRequests)}</span> / 错误 <span title={formatNumber(summary.errorRequests)}>{formatCompact(summary.errorRequests)}</span>
            </div>
          </div>
        </div>

        {/* 错误率 */}
        <StatCard
          title="错误率"
          value={formatPercent(summary.errorRate)}
          desc={summary.errorRequests > 0 ? '需查看异常摘要' : '当前窗口无错误'}
          icon={summary.errorRequests > 0 ? <ShieldAlert /> : <CheckCircle2 />}
          tone={summary.errorRate >= 0.2 ? 'error' : summary.errorRate > 0 ? 'warning' : 'success'}
        />

        {/* P95 耗时 */}
        <StatCard
          title="P95 耗时"
          value={formatDuration(summary.p95DurationMs)}
          desc={`均值 ${formatDuration(summary.averageDurationMs)}`}
          icon={<Clock3 />}
          tone={latencyTone}
        />

        {/* Token 用量 */}
        <StatCard
          title="Token"
          value={formatCompact(totalTokens)}
          valueTitle={formatNumber(totalTokens)}
          desc={`输入 ${formatCompact(summary.totalInputTokens)} / 输出 ${formatCompact(summary.totalOutputTokens)}`}
          icon={<Database />}
          tone="primary"
        />

        {/* 缓存命中 + Sparkline */}
        <div className="relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl border border-border bg-card p-4 shadow-sm transition-colors hover:border-border-strong">
          <span className="absolute left-0 top-4 h-8 w-1 rounded-r-full bg-success" />
          <div className="flex items-start justify-between gap-2 pl-2.5">
            <div className="min-w-0 flex-1">
              <div className="text-[0.72rem] font-semibold text-muted-foreground">缓存命中率</div>
              <div className="mt-1 text-2xl font-semibold tracking-tight tabular-nums text-success">
                {formatPercent(summary.cacheReadRatio)}
              </div>
            </div>
            <div className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-border bg-muted text-success [&_svg]:size-4">
              <Zap />
            </div>
          </div>
          <div className="mt-1 pl-2.5 truncate text-[0.72rem] text-muted-foreground" title={`${formatNumber(summary.totalCacheReadInputTokens)} tokens`}>
            读取 {formatCompact(summary.totalCacheReadInputTokens)} tokens
          </div>
        </div>

        {/* 估算费用 */}
        <StatCard
          title="估算费用"
          value={formatUsd(summary.totalEstimatedCostUsd)}
          desc={`计价覆盖 ${formatPercent(pricedRatio)}`}
          icon={<DollarSign />}
          tone="primary"
        />
      </div>

      {/* 2. 账号池状态 */}
      <CredentialPoolPanel />

      {/* 4. 趋势图区 */}
      <TrendSection hourly={series.hourly24h ?? []} daily={series.daily7d ?? []} />

      {/* 5. 运行信号 + 异常摘要 */}
      <div className="grid gap-3 xl:grid-cols-[1.1fr_0.9fr]">
        <RunSignalsPanel
          pricedRatio={pricedRatio}
          streamRatio={streamRatio}
          cacheReadRatio={summary.cacheReadRatio}
          stickyBound={summary.stickyBoundRequests}
          stickyFallback={summary.fallbackFromStickyRequests}
          simulated={summary.simulatedRequests}
          upstreamMeta={summary.upstreamMetadataRequests}
        />
        <ErrorSummaryPanel
          totalErrors={summary.errorRequests}
          errorRate={summary.errorRate}
          items={top.errors ?? []}
        />
      </div>

      {/* 6. 维度排行 */}
      <DimensionRankPanel top={top} activeKey={rankDimension} onActiveKeyChange={setRankDimension} />

      {/* 7. 外部账号计费拆分（始终展示，无样本时为持平/0） */}
      <ExternalPoolBillingPanel
        billing={summary.externalPoolBilling ?? EMPTY_EXTERNAL_POOL_BILLING}
        billingByPool={summary.externalPoolBillingByPool ?? []}
      />

      {/* 8. 状态分布 + 用量来源（Tab 切换） */}
      <BreakdownTabPanel
        statusItems={summary.statusBreakdown ?? []}
        sourceItems={summary.usageSourceBreakdown ?? []}
      />

      {/* 9. 底部状态栏 */}
      <div className="rounded-xl border border-border bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
        <div className="flex flex-wrap items-center gap-2">
          <span>总览 · {selectedWindow.label}</span>
          <span className="text-muted-foreground/40">·</span>
          <span>{formatDate(selectedWindow.from)} — {formatDate(selectedWindow.to)}</span>
          <span className="text-muted-foreground/40">·</span>
          <span>时区 {OVERVIEW_TIMEZONE}</span>
          <span className="text-muted-foreground/40">·</span>
          <span>
            {autoRefresh.enabled
              ? `每 ${autoRefresh.intervalSeconds} 秒自动刷新`
              : '自动刷新已关闭'}
          </span>
          {data.generatedAt && (
            <>
              <span className="text-muted-foreground/40">·</span>
              <span>生成于 {formatDate(data.generatedAt)}</span>
            </>
          )}
        </div>
      </div>
    </PageContainer>
  )
}
