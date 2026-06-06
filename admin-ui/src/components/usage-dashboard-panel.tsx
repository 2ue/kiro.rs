import { useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import { Activity, BarChart3, Clock3, DollarSign, RefreshCw, Server, Zap } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { useUsageDashboard } from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
import type {
  UsageBreakdownItem,
  UsageDashboardWindow,
  UsageSeriesPoint,
  UsageTopAggregate,
} from '@/types/api'

const DASHBOARD_TIMEZONE = 'Asia/Shanghai'

function formatNumber(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '0'
  return new Intl.NumberFormat('zh-CN').format(value as number)
}

function formatPercent(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return `${((value as number) * 100).toFixed(1)}%`
}

function formatUsd(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const number = value as number
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: number >= 1 ? 2 : 6,
    maximumFractionDigits: number >= 1 ? 2 : 6,
  }).format(number)
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
  tone?: 'default' | 'success' | 'warning' | 'info'
}) {
  const toneClass =
    tone === 'success'
      ? 'text-green-600 dark:text-green-400'
      : tone === 'warning'
        ? 'text-yellow-600 dark:text-yellow-400'
        : tone === 'info'
          ? 'text-blue-600 dark:text-blue-400'
          : 'text-foreground'
  return (
    <Card>
      <CardContent className="flex items-start justify-between gap-3 p-4">
        <div className="min-w-0">
          <div className="text-xs font-medium text-muted-foreground">{title}</div>
          <div className={`mt-1 truncate text-2xl font-semibold ${toneClass}`}>{value}</div>
          {desc && <div className="mt-1 truncate text-xs text-muted-foreground">{desc}</div>}
        </div>
        <div className="shrink-0 text-muted-foreground">{icon}</div>
      </CardContent>
    </Card>
  )
}

function SeriesChart({ title, points }: { title: string; points: UsageSeriesPoint[] }) {
  const maxRequests = Math.max(...points.map((point) => point.requests), 1)
  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-sm">{title}</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="flex h-36 items-end gap-1 overflow-x-auto pb-1">
          {points.map((point) => {
            const height = Math.max(4, Math.round((point.requests / maxRequests) * 120))
            return (
              <div key={point.key} className="flex min-w-9 flex-1 flex-col items-center gap-1">
                <div
                  className="w-full rounded-t bg-blue-500/75 transition-colors hover:bg-blue-500"
                  style={{ height }}
                  title={`${point.label}: ${formatNumber(point.requests)} 请求 / ${formatUsd(point.totalEstimatedCostUsd)}`}
                />
                <span className="w-full truncate text-center text-[10px] text-muted-foreground">{point.label}</span>
              </div>
            )
          })}
        </div>
      </CardContent>
    </Card>
  )
}

function BreakdownList({ title, items }: { title: string; items: UsageBreakdownItem[] }) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-sm">{title}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {items.length === 0 && <div className="text-sm text-muted-foreground">暂无数据</div>}
        {items.map((item) => (
          <div key={item.key} className="space-y-1">
            <div className="flex items-center justify-between gap-3 text-sm">
              <span className="truncate">{item.label}</span>
              <span className="shrink-0 font-mono text-xs text-muted-foreground">
                {formatNumber(item.requests)} · {formatPercent(item.ratio)}
              </span>
            </div>
            <div className="h-1.5 overflow-hidden rounded-full bg-secondary">
              <div className="h-full rounded-full bg-primary" style={{ width: `${Math.min(100, item.ratio * 100)}%` }} />
            </div>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}

function TopList({ title, items }: { title: string; items: UsageTopAggregate[] }) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-sm">{title}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-2">
        {items.length === 0 && <div className="text-sm text-muted-foreground">暂无数据</div>}
        {items.map((item, index) => (
          <div key={`${item.key}-${index}`} className="rounded-md border bg-muted/30 px-3 py-2">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="truncate text-sm font-medium" title={item.label || item.key}>
                  {item.label || item.key}
                </div>
                {item.label && <div className="truncate font-mono text-[11px] text-muted-foreground">{item.key}</div>}
              </div>
              <Badge variant={item.errorRequests > 0 ? 'warning' : 'secondary'}>{formatNumber(item.requests)}</Badge>
            </div>
            <div className="mt-2 grid grid-cols-3 gap-2 text-[11px] text-muted-foreground">
              <span>输入 {formatNumber(item.totalInputTokens)}</span>
              <span>输出 {formatNumber(item.totalOutputTokens)}</span>
              <span>{formatUsd(item.totalEstimatedCostUsd)}</span>
            </div>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}

export function UsageDashboardPanel() {
  const dashboard = useUsageDashboard(DASHBOARD_TIMEZONE)
  const [selectedWindowKey, setSelectedWindowKey] = useState('today')
  const data = dashboard.data
  const selectedWindow = useMemo(
    () => activeWindow(data?.windows || [], selectedWindowKey),
    [data?.windows, selectedWindowKey]
  )

  if (dashboard.isLoading) {
    return <div className="py-12 text-center text-sm text-muted-foreground">正在加载用量总览...</div>
  }

  if (dashboard.error) {
    return (
      <Card>
        <CardContent className="p-4 text-sm text-destructive">
          用量总览加载失败：{extractErrorMessage(dashboard.error)}
        </CardContent>
      </Card>
    )
  }

  if (!data || !selectedWindow) {
    return <div className="py-12 text-center text-sm text-muted-foreground">暂无用量数据</div>
  }

  const summary = selectedWindow.summary
  const pricedRatio = summary.totalRequests > 0 ? summary.pricedRequests / summary.totalRequests : 0
  const streamRatio = summary.totalRequests > 0 ? summary.streamRequests / summary.totalRequests : 0

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="text-xl font-semibold tracking-tight">用量总览</h2>
            <Badge variant="outline">{data.timezone}</Badge>
          </div>
          <p className="mt-1 text-sm text-muted-foreground">
            {selectedWindow.label}: {formatDate(selectedWindow.from)} - {formatDate(selectedWindow.to)}
          </p>
        </div>
        <Button type="button" variant="outline" size="sm" onClick={() => dashboard.refetch()}>
          <RefreshCw className="h-4 w-4" />
          刷新
        </Button>
      </div>

      <div className="flex flex-wrap gap-2">
        {data.windows.map((window) => (
          <Button
            key={window.key}
            type="button"
            size="sm"
            variant={window.key === selectedWindow.key ? 'default' : 'outline'}
            onClick={() => setSelectedWindowKey(window.key)}
          >
            {window.label}
          </Button>
        ))}
      </div>

      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
        <MetricCard title="请求数" value={formatNumber(summary.totalRequests)} desc={`成功 ${formatNumber(summary.successRequests)}`} icon={<Activity className="h-5 w-5" />} tone="info" />
        <MetricCard title="错误率" value={formatPercent(summary.errorRate)} desc={`错误 ${formatNumber(summary.errorRequests)}`} icon={<Zap className="h-5 w-5" />} tone={summary.errorRate > 0 ? 'warning' : 'success'} />
        <MetricCard title="Token" value={formatNumber(summary.totalInputTokens + summary.totalOutputTokens)} desc={`计费输入 ${formatNumber(summary.billableInputTokens)}`} icon={<BarChart3 className="h-5 w-5" />} />
        <MetricCard title="估算费用" value={formatUsd(summary.totalEstimatedCostUsd)} desc={`已计价 ${formatPercent(pricedRatio)}`} icon={<DollarSign className="h-5 w-5" />} tone="info" />
        <MetricCard title="缓存读取" value={formatNumber(summary.totalCacheReadInputTokens)} desc={`读取率 ${formatPercent(summary.cacheReadRatio)}`} icon={<Server className="h-5 w-5" />} tone="success" />
        <MetricCard title="耗时" value={`${Math.round(summary.averageDurationMs)}ms`} desc={`P95 ${formatNumber(summary.p95DurationMs)}ms`} icon={<Clock3 className="h-5 w-5" />} />
        <MetricCard title="Stream" value={formatPercent(streamRatio)} desc={`stream ${formatNumber(summary.streamRequests)}`} icon={<Activity className="h-5 w-5" />} />
        <MetricCard title="Sticky fallback" value={formatNumber(summary.fallbackFromStickyRequests)} desc={`sticky ${formatNumber(summary.stickyBoundRequests)}`} icon={<RefreshCw className="h-5 w-5" />} tone={summary.fallbackFromStickyRequests > 0 ? 'warning' : 'default'} />
      </div>

      <div className="grid gap-3 xl:grid-cols-2">
        <SeriesChart title="最近 24 小时请求趋势" points={data.series.hourly24h} />
        <SeriesChart title="最近 7 天请求趋势" points={data.series.daily7d} />
      </div>

      <div className="grid gap-3 xl:grid-cols-2">
        <BreakdownList title="状态分布" items={summary.statusBreakdown} />
        <BreakdownList title="用量来源" items={summary.usageSourceBreakdown} />
      </div>

      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
        <TopList title="Top 模型（24h）" items={data.top.models} />
        <TopList title="Top 账号（24h）" items={data.top.credentials} />
        <TopList title="Top Endpoint（24h）" items={data.top.endpoints} />
        <TopList title="Top 错误（24h）" items={data.top.errors} />
      </div>
    </div>
  )
}
