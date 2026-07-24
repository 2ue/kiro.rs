import { useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import {
  Activity,
  AlertTriangle,
  BarChart3,
  CheckCircle2,
  Clock3,
  Database,
  DollarSign,
  Gauge,
  LineChart,
  ShieldAlert,
  Zap,
} from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import {
  useUsageDashboardBreakdown,
  useUsageDashboardExternalPoolBilling,
  useUsageDashboardSeries,
  useUsageDashboardTop,
  useUsageDashboardWindows,
} from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
import { formatUsd } from '@/lib/format'
import type {
  UsageBreakdownItem,
  UsageDashboardWindowsResponse,
  UsageDashboardWindow,
  UsageExternalPoolBillingByPool,
  UsageExternalPoolBillingSummary,
  UsageSeriesPoint,
  UsageTopAggregate,
} from '@/types/api'

const DASHBOARD_TIMEZONE = 'Asia/Shanghai'
const DASHBOARD_AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:dashboard'

type DashboardTone = 'default' | 'success' | 'warning' | 'error' | 'info'
type RankDimension = 'models' | 'credentials' | 'endpoints' | 'errors'
type BillingDeltaTone = 'loss' | 'profit' | 'even'

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

const EMPTY_TOP = {
  models: [] as UsageTopAggregate[],
  credentials: [] as UsageTopAggregate[],
  endpoints: [] as UsageTopAggregate[],
  errors: [] as UsageTopAggregate[],
}

const rankDimensions: Array<{ key: RankDimension; label: string }> = [
  { key: 'models', label: '模型' },
  { key: 'credentials', label: '账号' },
  { key: 'endpoints', label: 'Endpoint' },
  { key: 'errors', label: '错误' },
]

function formatNumber(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '0'
  return new Intl.NumberFormat('zh-CN').format(value as number)
}

function formatPercent(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return `${((value as number) * 100).toFixed(1)}%`
}

function formatDate(value?: string): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function activeWindow(windows: UsageDashboardWindow[], key: string): UsageDashboardWindow | undefined {
  return windows.find((window) => window.key === key) || windows[0]
}

function errorRateTone(errorRate: number): DashboardTone {
  if (errorRate >= 0.2) return 'error'
  if (errorRate > 0) return 'warning'
  return 'success'
}

function billingDeltaTone(delta: number): BillingDeltaTone {
  if (delta < 0) return 'loss'
  if (delta > 0) return 'profit'
  return 'even'
}

function billingDeltaTextClass(tone: BillingDeltaTone): string {
  if (tone === 'loss') return 'text-kiro-error'
  if (tone === 'profit') return 'text-kiro-warning'
  return 'text-muted-foreground'
}

function toneText(tone: DashboardTone): string {
  if (tone === 'success') return 'text-kiro-success'
  if (tone === 'warning') return 'text-kiro-warning'
  if (tone === 'error') return 'text-kiro-error'
  if (tone === 'info') return 'text-kiro-info'
  return 'text-foreground'
}

function toneBar(tone: DashboardTone): string {
  if (tone === 'success') return 'bg-kiro-success'
  if (tone === 'warning') return 'bg-kiro-warning'
  if (tone === 'error') return 'bg-kiro-error'
  if (tone === 'info') return 'bg-kiro-info'
  return 'bg-primary'
}

function MetricCard({
  title,
  value,
  desc,
  icon,
  tone = 'default',
}: {
  title: string
  value: string
  desc?: string
  icon: ReactNode
  tone?: DashboardTone
}) {
  return (
    <Card>
      <CardContent className="flex items-start justify-between gap-3 p-4">
        <div className="min-w-0">
          <div className="text-xs font-medium text-muted-foreground">{title}</div>
          <div className={`mt-1 truncate text-2xl font-semibold ${toneText(tone)}`}>{value}</div>
          {desc && <div className="mt-1 truncate text-xs text-muted-foreground">{desc}</div>}
        </div>
        <div className="shrink-0 text-muted-foreground">{icon}</div>
      </CardContent>
    </Card>
  )
}

function Panel({
  title,
  subtitle,
  actions,
  children,
}: {
  title: string
  subtitle?: ReactNode
  actions?: ReactNode
  children: ReactNode
}) {
  return (
    <Card className="overflow-hidden">
      <CardHeader className="flex flex-col gap-1.5 space-y-0 border-b p-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <CardTitle className="truncate text-sm">{title}</CardTitle>
          {subtitle && <div className="text-xs text-muted-foreground">{subtitle}</div>}
        </div>
        {actions && <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>}
      </CardHeader>
      <CardContent className="p-4">{children}</CardContent>
    </Card>
  )
}

function DashboardToolbar({
  data,
  selectedWindow,
  onWindowChange,
  autoRefreshEnabled,
  autoRefreshSeconds,
  onAutoRefreshEnabledChange,
  onAutoRefreshSecondsChange,
}: {
  data: UsageDashboardWindowsResponse
  selectedWindow: UsageDashboardWindow
  onWindowChange: (key: string) => void
  autoRefreshEnabled: boolean
  autoRefreshSeconds: number
  onAutoRefreshEnabledChange: (enabled: boolean) => void
  onAutoRefreshSecondsChange: (seconds: number) => void
}) {
  return (
    <div className="rounded-lg border bg-card px-4 py-3 shadow-sm">
      <div className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold">{selectedWindow.label}</span>
            <Badge variant="outline">{data.timezone}</Badge>
            <Badge variant={autoRefreshEnabled ? 'secondary' : 'outline'}>
              {autoRefreshEnabled ? `自动刷新 ${autoRefreshSeconds}s` : '自动刷新关闭'}
            </Badge>
          </div>
          <div className="mt-1 text-xs text-muted-foreground">
            {formatDate(selectedWindow.from)} - {formatDate(selectedWindow.to)} · 生成 {formatDate(data.generatedAt)}
          </div>
        </div>

        <div className="flex flex-col gap-2 xl:items-end">
          <div className="flex flex-wrap gap-2">
            {data.windows.map((window) => (
              <Button
                key={window.key}
                type="button"
                size="sm"
                variant={window.key === selectedWindow.key ? 'default' : 'outline'}
                onClick={() => onWindowChange(window.key)}
              >
                {window.label}
              </Button>
            ))}
          </div>
          <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <label className="flex cursor-pointer items-center gap-2">
              <input
                type="checkbox"
                className="h-4 w-4"
                checked={autoRefreshEnabled}
                onChange={(event) => onAutoRefreshEnabledChange(event.target.checked)}
              />
              自动刷新
            </label>
            <Input
              type="number"
              min={5}
              max={3600}
              className="h-8 w-20"
              value={autoRefreshSeconds}
              disabled={!autoRefreshEnabled}
              onChange={(event) => onAutoRefreshSecondsChange(Number(event.target.value))}
            />
            <span>秒</span>
          </div>
        </div>
      </div>
    </div>
  )
}

function SeriesChart({ title, points }: { title: string; points: UsageSeriesPoint[] }) {
  const maxRequests = Math.max(...points.map((point) => point.requests), 1)
  const totalRequests = points.reduce((sum, point) => sum + point.requests, 0)
  const totalCost = points.reduce((sum, point) => sum + point.totalEstimatedCostUsd, 0)
  const totalOriginalCost = points.reduce((sum, point) => sum + (point.totalOriginalCostUsd ?? 0), 0)
  const totalErrors = points.reduce((sum, point) => sum + point.errorRequests, 0)

  return (
    <Panel
      title={title}
      subtitle={`${formatNumber(totalRequests)} 请求 · 估算 ${formatUsd(totalCost)} · 原始 ${formatUsd(totalOriginalCost)}`}
      actions={<Badge variant={totalErrors > 0 ? 'destructive' : 'success'}>{totalErrors > 0 ? `错误 ${formatNumber(totalErrors)}` : '无错误'}</Badge>}
    >
      <div className="overflow-x-auto">
        {points.length === 0 ? (
          <div className="flex h-32 items-center justify-center text-sm text-muted-foreground">暂无数据</div>
        ) : (
          <div className="flex h-32 min-w-[520px] items-end gap-1.5">
            {points.map((point) => {
              const height = Math.max(6, Math.round((point.requests / maxRequests) * 96))
              const errorHeight = point.requests > 0
                ? Math.max(point.errorRequests > 0 ? 4 : 0, Math.round((point.errorRequests / point.requests) * height))
                : 0
              return (
                <div key={point.key} className="group flex min-w-0 flex-1 flex-col items-center gap-1">
                  <div
                    className="relative w-full overflow-hidden rounded-t bg-primary/70 transition-colors group-hover:bg-primary"
                    style={{ height }}
                    title={`${point.label}: ${formatNumber(point.requests)} 请求 / ${formatNumber(point.errorRequests)} 错误 / 估算 ${formatUsd(point.totalEstimatedCostUsd)} / 原始 ${formatUsd(point.totalOriginalCostUsd)}`}
                  >
                    {errorHeight > 0 && <div className="absolute inset-x-0 bottom-0 bg-kiro-error" style={{ height: errorHeight }} />}
                  </div>
                  <span className="w-full truncate text-center text-[10px] text-muted-foreground">{point.label}</span>
                </div>
              )
            })}
          </div>
        )}
      </div>
    </Panel>
  )
}

function SignalRow({
  label,
  value,
  ratio,
  tone = 'info',
}: {
  label: string
  value: ReactNode
  ratio?: number
  tone?: DashboardTone
}) {
  const width = Number.isFinite(ratio ?? Number.NaN) ? Math.min(100, Math.max(0, (ratio as number) * 100)) : 0

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2 text-sm">
        <span className="truncate font-medium">{label}</span>
        <span className="shrink-0 font-mono text-xs text-muted-foreground">{value}</span>
      </div>
      {ratio !== undefined && (
        <div className="h-1.5 overflow-hidden rounded-full bg-secondary">
          <div className={`h-full rounded-full ${toneBar(tone)}`} style={{ width: `${width}%` }} />
        </div>
      )}
    </div>
  )
}

function BreakdownPanel({
  title,
  subtitle,
  items,
  emptyText,
}: {
  title: string
  subtitle: string
  items: UsageBreakdownItem[]
  emptyText: string
}) {
  return (
    <Panel title={title} subtitle={subtitle}>
      <div className="space-y-3">
        {items.length === 0 ? (
          <div className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">{emptyText}</div>
        ) : (
          items.slice(0, 6).map((item) => (
            <SignalRow
              key={item.key}
              label={item.label}
              value={`${formatNumber(item.requests)} · ${formatPercent(item.ratio)}`}
              ratio={item.ratio}
              tone={item.key === 'success' ? 'success' : item.key.includes('timeout') || item.key.includes('error') ? 'error' : 'info'}
            />
          ))
        )}
      </div>
    </Panel>
  )
}

function ErrorFocusPanel({
  totalErrors,
  items,
}: {
  totalErrors: number
  items: UsageTopAggregate[]
}) {
  const visibleItems = items.filter((item) => item.requests > 0).slice(0, 4)

  return (
    <Panel
      title="异常摘要"
      subtitle="只展示需要排障的错误聚合，完整明细到用量记录页筛选"
      actions={<Badge variant={totalErrors > 0 ? 'destructive' : 'success'}>{totalErrors > 0 ? `${formatNumber(totalErrors)} 错误` : '正常'}</Badge>}
    >
      <div className="space-y-3">
        {visibleItems.length === 0 ? (
          <div className="flex items-center gap-2 rounded-md border border-kiro-success-soft bg-kiro-success-soft p-3 text-sm text-kiro-success">
            <CheckCircle2 className="h-4 w-4 shrink-0" />
            当前窗口没有错误聚合。
          </div>
        ) : (
          visibleItems.map((item, index) => (
            <div key={`${item.key}-${index}`} className="rounded-md border border-kiro-error-soft bg-kiro-error-soft p-3">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-sm font-semibold text-kiro-error" title={item.label || item.key}>
                    {item.label || item.key}
                  </div>
                  {item.label && <div className="truncate font-mono text-[11px] text-muted-foreground">{item.key}</div>}
                </div>
                <Badge variant="destructive">{formatNumber(item.requests)}</Badge>
              </div>
              <div className="mt-2 grid grid-cols-3 gap-2 text-[11px] text-muted-foreground">
                <span className="truncate">输入 {formatNumber(item.totalInputTokens)}</span>
                <span className="truncate">输出 {formatNumber(item.totalOutputTokens)}</span>
                <span className="truncate text-right" title={`估算 ${formatUsd(item.totalEstimatedCostUsd)} / 原始 ${formatUsd(item.totalOriginalCostUsd)}`}>
                  原始 {formatUsd(item.totalOriginalCostUsd)}
                </span>
              </div>
            </div>
          ))
        )}
      </div>
    </Panel>
  )
}

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
  onActiveKeyChange: (key: RankDimension) => void
}) {
  const items = top[activeKey] || []
  const totalRequests = items.reduce((sum, item) => sum + item.requests, 0)

  return (
    <Panel
      title="维度排行"
      subtitle="保留后端 Top 聚合，切换维度查看，不占用总览主视图空间"
      actions={
        <div className="flex flex-wrap gap-2">
          {rankDimensions.map((dimension) => (
            <Button
              key={dimension.key}
              type="button"
              size="sm"
              variant={dimension.key === activeKey ? 'default' : 'outline'}
              onClick={() => onActiveKeyChange(dimension.key)}
            >
              {dimension.label}
            </Button>
          ))}
        </div>
      }
    >
      <div className="divide-y">
        {items.length === 0 ? (
          <div className="py-3 text-sm text-muted-foreground">暂无排行数据。</div>
        ) : (
          items.map((item, index) => (
            <div key={`${activeKey}-${item.key}-${index}`} className="grid gap-2 py-2.5 md:grid-cols-[minmax(0,1fr)_220px] md:items-center">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded bg-muted font-mono text-[11px] font-semibold text-muted-foreground">
                    {index + 1}
                  </span>
                  <span className="truncate text-sm font-semibold" title={item.label || item.key}>
                    {item.label || item.key}
                  </span>
                  {item.errorRequests > 0 && <Badge variant="destructive">错 {formatNumber(item.errorRequests)}</Badge>}
                </div>
                {item.label && <div className="mt-0.5 truncate pl-7 font-mono text-[11px] text-muted-foreground">{item.key}</div>}
                <div className="mt-1.5 pl-7">
                  <div className="h-1.5 overflow-hidden rounded-full bg-secondary">
                    <div className="h-full rounded-full bg-primary" style={{ width: `${totalRequests > 0 ? Math.min(100, (item.requests / totalRequests) * 100) : 0}%` }} />
                  </div>
                </div>
              </div>
              <div className="grid grid-cols-5 gap-2 text-right text-[11px] text-muted-foreground">
                <span className="truncate">请求 {formatNumber(item.requests)}</span>
                <span className="truncate">输入 {formatNumber(item.totalInputTokens)}</span>
                <span className="truncate">输出 {formatNumber(item.totalOutputTokens)}</span>
                <span className="truncate">估算 {formatUsd(item.totalEstimatedCostUsd)}</span>
                <span className="truncate">原始 {formatUsd(item.totalOriginalCostUsd)}</span>
              </div>
            </div>
          ))
        )}
      </div>
    </Panel>
  )
}

function OperationsPanel({
  pricedRatio,
  streamRatio,
  cacheReadRatio,
  stickyBoundRequests,
  fallbackFromStickyRequests,
  simulatedRequests,
  upstreamMetadataRequests,
}: {
  pricedRatio: number
  streamRatio: number
  cacheReadRatio: number
  stickyBoundRequests: number
  fallbackFromStickyRequests: number
  simulatedRequests: number
  upstreamMetadataRequests: number
}) {
  return (
    <Panel title="运行信号" subtitle="这些指标更适合总览页判断配置和调度是否异常">
      <div className="grid gap-3 md:grid-cols-2">
        <SignalRow label="计价覆盖" value={formatPercent(pricedRatio)} ratio={pricedRatio} tone={pricedRatio < 1 ? 'error' : 'success'} />
        <SignalRow label="流式占比" value={formatPercent(streamRatio)} ratio={streamRatio} tone="info" />
        <SignalRow label="缓存读取率" value={formatPercent(cacheReadRatio)} ratio={cacheReadRatio} tone="success" />
        <SignalRow
          label="Sticky 回退"
          value={`${formatNumber(fallbackFromStickyRequests)} / sticky ${formatNumber(stickyBoundRequests)}`}
          ratio={stickyBoundRequests > 0 ? fallbackFromStickyRequests / stickyBoundRequests : 0}
          tone={fallbackFromStickyRequests > 0 ? 'warning' : 'default'}
        />
        <SignalRow label="模拟用量" value={formatNumber(simulatedRequests)} tone={simulatedRequests > 0 ? 'info' : 'default'} />
        <SignalRow label="上游元数据" value={formatNumber(upstreamMetadataRequests)} tone="info" />
      </div>
    </Panel>
  )
}

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
    <Panel
      title="备用池计费拆分"
      subtitle="上游原始成本来自外部池原始 usage；最终上报使用整形并放大后的 usage；盈利按放大后计费减上游原始成本计算"
      actions={
        <Badge variant={hasLoss ? 'destructive' : hasProfit ? 'warning' : 'success'}>
          {hasLoss ? `亏损 ${formatUsd(Math.abs(profit))}` : hasProfit ? `盈利 ${formatUsd(profit)}` : '持平'}
        </Badge>
      }
    >
      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">外部池请求</div>
          <div className="mt-1 text-lg font-semibold">{formatNumber(billing.requests)}</div>
          <div className="mt-1 text-xs text-muted-foreground">可计价 {formatNumber(billing.pricedRequests)} / 未计价 {formatNumber(billing.unpricedRequests)}</div>
        </div>
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">上游原始成本</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(billing.rawCostUsd)}</div>
          <div className="mt-1 text-xs text-muted-foreground">按备用池 raw usage 估算</div>
        </div>
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">整形后计费</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(shapedCost)}</div>
          <div className="mt-1 text-xs text-muted-foreground">路径缓存整形后，未放大</div>
        </div>
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">整形后放大计费</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(upliftedCost)}</div>
          <div className={`mt-1 text-xs ${billingDeltaTextClass(deltaTone)}`}>
            盈利 = 放大后 - 上游原始：{profit >= 0 ? '+' : ''}{formatUsd(profit)}
          </div>
        </div>
      </div>
      <div className="mt-3">
        <SignalRow
          label="盈利占上游原始成本"
          value={`${profit >= 0 ? '+' : ''}${formatUsd(profit)} · ${formatPercent(profitRatio)}`}
          ratio={Math.abs(profitRatio)}
          tone={hasLoss ? 'error' : 'success'}
        />
      </div>
      <div className="mt-4 border-t pt-4">
        <div className="mb-2 flex items-center justify-between gap-2">
          <div className="text-xs font-semibold text-muted-foreground">分号池成本与盈亏</div>
          <div className="text-[0.68rem] text-muted-foreground">按当前时间窗口聚合</div>
        </div>
        {visiblePools.length === 0 ? (
          <div className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">
            当前窗口没有备用池计费样本。
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[780px] text-xs">
              <thead className="border-b text-muted-foreground">
                <tr>
                  <th className="py-2 text-left font-medium">号池</th>
                  <th className="py-2 text-right font-medium">请求</th>
                  <th className="py-2 text-right font-medium">上游原始成本</th>
                  <th className="py-2 text-right font-medium">整形后</th>
                  <th className="py-2 text-right font-medium">放大后</th>
                  <th className="py-2 text-right font-medium">盈亏</th>
                  <th className="py-2 text-right font-medium">未计价</th>
                  <th className="py-2 text-right font-medium">兜底</th>
                </tr>
              </thead>
              <tbody className="divide-y">
                {visiblePools.map((pool) => {
                  const poolProfit = pool.profitUsd ?? ((pool.upliftedCostUsd ?? pool.reportedCostUsd ?? 0) - pool.rawCostUsd)
                  const poolTone = billingDeltaTone(poolProfit)
                  return (
                    <tr key={pool.poolId}>
                      <td className="py-2">
                        <div className="max-w-[240px] truncate font-medium" title={pool.poolName}>{pool.poolName}</div>
                        <div className="font-mono text-[0.65rem] text-muted-foreground">#{pool.poolId}</div>
                      </td>
                      <td className="py-2 text-right font-mono">{formatNumber(pool.requests)}</td>
                      <td className="py-2 text-right font-mono">{formatUsd(pool.rawCostUsd)}</td>
                      <td className="py-2 text-right font-mono">{formatUsd(pool.shapedCostUsd ?? pool.reportedCostUsd)}</td>
                      <td className="py-2 text-right font-mono">{formatUsd(pool.upliftedCostUsd ?? pool.reportedCostUsd)}</td>
                      <td className={`py-2 text-right font-mono ${billingDeltaTextClass(poolTone)}`}>
                        {poolProfit >= 0 ? '+' : ''}{formatUsd(poolProfit)}
                      </td>
                      <td className="py-2 text-right font-mono">{formatNumber(pool.unpricedRequests)}</td>
                      <td className="py-2 text-right font-mono">{formatNumber(pool.costFloorAppliedRequests)}</td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </Panel>
  )
}

export function UsageDashboardPanel() {
  const autoRefresh = useAutoRefreshPreference(DASHBOARD_AUTO_REFRESH_KEY)
  const windowsQuery = useUsageDashboardWindows(DASHBOARD_TIMEZONE, autoRefresh.refetchInterval)
  const [selectedWindowKey, setSelectedWindowKey] = useState('today')
  const [rankDimension, setRankDimension] = useState<RankDimension>('credentials')
  const data = windowsQuery.data
  const selectedWindow = useMemo(
    () => activeWindow(data?.windows || [], selectedWindowKey),
    [data?.windows, selectedWindowKey]
  )
  const effectiveWindowKey = selectedWindow?.key || selectedWindowKey
  const seriesQuery = useUsageDashboardSeries(DASHBOARD_TIMEZONE, autoRefresh.refetchInterval)
  const topQuery = useUsageDashboardTop(autoRefresh.refetchInterval)
  const breakdownQuery = useUsageDashboardBreakdown(
    DASHBOARD_TIMEZONE,
    effectiveWindowKey,
    autoRefresh.refetchInterval
  )
  const externalPoolBillingQuery = useUsageDashboardExternalPoolBilling(
    DASHBOARD_TIMEZONE,
    effectiveWindowKey,
    autoRefresh.refetchInterval
  )

  if (windowsQuery.isLoading) {
    return <div className="py-12 text-center text-sm text-muted-foreground">正在加载用量总览...</div>
  }

  if (windowsQuery.error) {
    return (
      <Card>
        <CardContent className="p-4 text-sm text-destructive">
          用量总览加载失败：{extractErrorMessage(windowsQuery.error)}
        </CardContent>
      </Card>
    )
  }

  if (!data || !selectedWindow) {
    return <div className="py-12 text-center text-sm text-muted-foreground">暂无用量数据</div>
  }

  const summary = selectedWindow.summary
  const top = topQuery.data?.top || EMPTY_TOP
  const series = seriesQuery.data?.series || { hourly24h: [], daily7d: [] }
  const externalPoolBilling = summary.externalPoolBilling || EMPTY_EXTERNAL_POOL_BILLING
  const externalPoolBillingByPool =
    externalPoolBillingQuery.data?.externalPoolBillingByPool || summary.externalPoolBillingByPool || []
  const statusBreakdown = breakdownQuery.data?.statusBreakdown || summary.statusBreakdown || []
  const usageSourceBreakdown = breakdownQuery.data?.usageSourceBreakdown || summary.usageSourceBreakdown || []
  const partialErrors = [
    seriesQuery.error ? `趋势：${extractErrorMessage(seriesQuery.error)}` : '',
    topQuery.error ? `排行：${extractErrorMessage(topQuery.error)}` : '',
    breakdownQuery.error ? `分布：${extractErrorMessage(breakdownQuery.error)}` : '',
    externalPoolBillingQuery.error ? `备用池计费：${extractErrorMessage(externalPoolBillingQuery.error)}` : '',
  ].filter(Boolean)
  const pricedRatio = summary.totalRequests > 0 ? summary.pricedRequests / summary.totalRequests : 0
  const streamRatio = summary.totalRequests > 0 ? summary.streamRequests / summary.totalRequests : 0
  const totalTokens = summary.totalInputTokens + summary.totalOutputTokens
  const latencyTone: DashboardTone = summary.p95DurationMs >= 60_000 ? 'warning' : 'default'

  return (
    <div className="space-y-4">
      <DashboardToolbar
        data={data}
        selectedWindow={selectedWindow}
        onWindowChange={setSelectedWindowKey}
        autoRefreshEnabled={autoRefresh.enabled}
        autoRefreshSeconds={autoRefresh.intervalSeconds}
        onAutoRefreshEnabledChange={autoRefresh.setEnabled}
        onAutoRefreshSecondsChange={autoRefresh.setIntervalSeconds}
      />

      {partialErrors.length > 0 && (
        <Card>
          <CardContent className="p-3 text-xs text-kiro-warning">
            部分数据加载失败：{partialErrors.join('；')}
          </CardContent>
        </Card>
      )}

      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-7">
        <MetricCard title="请求健康" value={formatNumber(summary.totalRequests)} desc={`成功 ${formatNumber(summary.successRequests)} / 错误 ${formatNumber(summary.errorRequests)}`} icon={<Activity className="h-5 w-5" />} tone={errorRateTone(summary.errorRate)} />
        <MetricCard title="错误率" value={formatPercent(summary.errorRate)} desc={summary.errorRequests > 0 ? '需要查看异常摘要' : '当前窗口无错误'} icon={summary.errorRequests > 0 ? <ShieldAlert className="h-5 w-5" /> : <CheckCircle2 className="h-5 w-5" />} tone={errorRateTone(summary.errorRate)} />
        <MetricCard title="耗时" value={`${Math.round(summary.averageDurationMs)}ms`} desc={`P95 ${formatNumber(summary.p95DurationMs)}ms`} icon={<Clock3 className="h-5 w-5" />} tone={latencyTone} />
        <MetricCard title="估算费用" value={formatUsd(summary.totalEstimatedCostUsd)} desc={`计价覆盖 ${formatPercent(pricedRatio)}`} icon={<DollarSign className="h-5 w-5" />} tone={pricedRatio < 1 && summary.totalRequests > 0 ? 'warning' : 'info'} />
        <MetricCard title="原始计费" value={formatUsd(summary.totalOriginalCostUsd)} desc="按上游原始 usage 估算" icon={<DollarSign className="h-5 w-5" />} tone="warning" />
        <MetricCard title="Token" value={formatNumber(totalTokens)} desc={`输入 ${formatNumber(summary.totalInputTokens)} / 输出 ${formatNumber(summary.totalOutputTokens)}`} icon={<BarChart3 className="h-5 w-5" />} />
        <MetricCard title="缓存读取" value={formatPercent(summary.cacheReadRatio)} desc={`读取 ${formatNumber(summary.totalCacheReadInputTokens)}`} icon={<Database className="h-5 w-5" />} tone="success" />
      </div>

      <div className="grid gap-3 xl:grid-cols-2">
        <SeriesChart title="最近 24 小时请求趋势" points={series.hourly24h || []} />
        <SeriesChart title="最近 7 天请求趋势" points={series.daily7d || []} />
      </div>

      <div className="grid gap-3 xl:grid-cols-[1.1fr_0.9fr]">
        <OperationsPanel
          pricedRatio={pricedRatio}
          streamRatio={streamRatio}
          cacheReadRatio={summary.cacheReadRatio}
          stickyBoundRequests={summary.stickyBoundRequests}
          fallbackFromStickyRequests={summary.fallbackFromStickyRequests}
          simulatedRequests={summary.simulatedRequests}
          upstreamMetadataRequests={summary.upstreamMetadataRequests}
        />
        <ErrorFocusPanel totalErrors={summary.errorRequests} items={top.errors || []} />
      </div>

      <ExternalPoolBillingPanel billing={externalPoolBilling} billingByPool={externalPoolBillingByPool} />

      <div className="grid gap-3 xl:grid-cols-2">
        <BreakdownPanel title="状态分布" subtitle="判断成功、上游超时、客户端错误等整体占比" items={statusBreakdown} emptyText="暂无状态样本。" />
        <BreakdownPanel title="用量来源" subtitle="判断真实上游、缓存模拟、补录等来源占比" items={usageSourceBreakdown} emptyText="暂无来源样本。" />
      </div>

      <DimensionRankPanel top={top} activeKey={rankDimension} onActiveKeyChange={setRankDimension} />

      <div className="rounded-lg border bg-card px-3 py-2.5 text-xs text-muted-foreground">
        <div className="flex flex-wrap items-center gap-2">
          <Gauge className="h-4 w-4" />
          <span>总览保留 Top 维度聚合；单条请求链路和更精确筛选请在“Usage”页查看。</span>
          <LineChart className="h-4 w-4" />
          <span>{autoRefresh.enabled ? `页面数据每 ${autoRefresh.intervalSeconds} 秒自动刷新。` : '自动刷新已关闭。'}</span>
          {summary.errorRequests > 0 && (
            <>
              <AlertTriangle className="h-4 w-4 text-kiro-error" />
              <span className="text-kiro-error">当前窗口存在错误请求，优先查看异常摘要和用量详情。</span>
            </>
          )}
          {summary.fallbackFromStickyRequests > 0 && (
            <>
              <Zap className="h-4 w-4 text-kiro-warning" />
              <span className="text-kiro-warning">检测到 Sticky 回退，说明粘度命中的账号不可用或并发不可用。</span>
            </>
          )}
        </div>
      </div>
    </div>
  )
}
