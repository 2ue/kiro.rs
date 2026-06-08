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
import { useUsageDashboard } from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
import type {
  UsageBreakdownItem,
  UsageDashboardWindow,
  UsageExternalPoolBillingSummary,
  UsageSeriesPoint,
  UsageTopAggregate,
} from '@/types/api'

const DASHBOARD_TIMEZONE = 'Asia/Shanghai'
const AUTO_REFRESH_SECONDS = 10

type DashboardTone = 'default' | 'success' | 'warning' | 'info'
type RankDimension = 'models' | 'credentials' | 'endpoints' | 'errors'

const EMPTY_EXTERNAL_POOL_BILLING: UsageExternalPoolBillingSummary = {
  requests: 0,
  pricedRequests: 0,
  unpricedRequests: 0,
  costFloorAppliedRequests: 0,
  rawCostUsd: 0,
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

function toneText(tone: DashboardTone): string {
  if (tone === 'success') return 'text-green-600 dark:text-green-400'
  if (tone === 'warning') return 'text-yellow-600 dark:text-yellow-400'
  if (tone === 'info') return 'text-blue-600 dark:text-blue-400'
  return 'text-foreground'
}

function toneBar(tone: DashboardTone): string {
  if (tone === 'success') return 'bg-green-500'
  if (tone === 'warning') return 'bg-yellow-500'
  if (tone === 'info') return 'bg-blue-500'
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
}: {
  data: NonNullable<ReturnType<typeof useUsageDashboard>['data']>
  selectedWindow: UsageDashboardWindow
  onWindowChange: (key: string) => void
}) {
  return (
    <div className="rounded-lg border bg-card px-4 py-3 shadow-sm">
      <div className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold">{selectedWindow.label}</span>
            <Badge variant="outline">{data.timezone}</Badge>
            <Badge variant="secondary">自动刷新 {AUTO_REFRESH_SECONDS}s</Badge>
          </div>
          <div className="mt-1 text-xs text-muted-foreground">
            {formatDate(selectedWindow.from)} - {formatDate(selectedWindow.to)} · 生成 {formatDate(data.generatedAt)}
          </div>
        </div>

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
      </div>
    </div>
  )
}

function SeriesChart({ title, points }: { title: string; points: UsageSeriesPoint[] }) {
  const maxRequests = Math.max(...points.map((point) => point.requests), 1)
  const totalRequests = points.reduce((sum, point) => sum + point.requests, 0)
  const totalCost = points.reduce((sum, point) => sum + point.totalEstimatedCostUsd, 0)
  const totalErrors = points.reduce((sum, point) => sum + point.errorRequests, 0)

  return (
    <Panel
      title={title}
      subtitle={`${formatNumber(totalRequests)} 请求 · ${formatUsd(totalCost)}`}
      actions={<Badge variant={totalErrors > 0 ? 'warning' : 'success'}>{totalErrors > 0 ? `错误 ${formatNumber(totalErrors)}` : '无错误'}</Badge>}
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
                    className="relative w-full overflow-hidden rounded-t bg-blue-500/75 transition-colors group-hover:bg-blue-500"
                    style={{ height }}
                    title={`${point.label}: ${formatNumber(point.requests)} 请求 / ${formatNumber(point.errorRequests)} 错误 / ${formatUsd(point.totalEstimatedCostUsd)}`}
                  >
                    {errorHeight > 0 && <div className="absolute inset-x-0 bottom-0 bg-yellow-500" style={{ height: errorHeight }} />}
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
              tone={item.key === 'success' ? 'success' : item.key.includes('timeout') || item.key.includes('error') ? 'warning' : 'info'}
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
      actions={<Badge variant={totalErrors > 0 ? 'warning' : 'success'}>{totalErrors > 0 ? `${formatNumber(totalErrors)} 错误` : '正常'}</Badge>}
    >
      <div className="space-y-3">
        {visibleItems.length === 0 ? (
          <div className="flex items-center gap-2 rounded-md border border-green-500/20 bg-green-500/5 p-3 text-sm text-green-600 dark:text-green-400">
            <CheckCircle2 className="h-4 w-4 shrink-0" />
            当前窗口没有错误聚合。
          </div>
        ) : (
          visibleItems.map((item, index) => (
            <div key={`${item.key}-${index}`} className="rounded-md border border-yellow-500/20 bg-yellow-500/5 p-3">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-sm font-semibold text-yellow-700 dark:text-yellow-400" title={item.label || item.key}>
                    {item.label || item.key}
                  </div>
                  {item.label && <div className="truncate font-mono text-[11px] text-muted-foreground">{item.key}</div>}
                </div>
                <Badge variant="warning">{formatNumber(item.requests)}</Badge>
              </div>
              <div className="mt-2 grid grid-cols-3 gap-2 text-[11px] text-muted-foreground">
                <span className="truncate">输入 {formatNumber(item.totalInputTokens)}</span>
                <span className="truncate">输出 {formatNumber(item.totalOutputTokens)}</span>
                <span className="truncate text-right">{formatUsd(item.totalEstimatedCostUsd)}</span>
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
                  {item.errorRequests > 0 && <Badge variant="warning">错 {formatNumber(item.errorRequests)}</Badge>}
                </div>
                {item.label && <div className="mt-0.5 truncate pl-7 font-mono text-[11px] text-muted-foreground">{item.key}</div>}
                <div className="mt-1.5 pl-7">
                  <div className="h-1.5 overflow-hidden rounded-full bg-secondary">
                    <div className="h-full rounded-full bg-primary" style={{ width: `${totalRequests > 0 ? Math.min(100, (item.requests / totalRequests) * 100) : 0}%` }} />
                  </div>
                </div>
              </div>
              <div className="grid grid-cols-4 gap-2 text-right text-[11px] text-muted-foreground">
                <span className="truncate">请求 {formatNumber(item.requests)}</span>
                <span className="truncate">输入 {formatNumber(item.totalInputTokens)}</span>
                <span className="truncate">输出 {formatNumber(item.totalOutputTokens)}</span>
                <span className="truncate">{formatUsd(item.totalEstimatedCostUsd)}</span>
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
        <SignalRow label="计价覆盖" value={formatPercent(pricedRatio)} ratio={pricedRatio} tone={pricedRatio < 1 ? 'warning' : 'success'} />
        <SignalRow label="流式占比" value={formatPercent(streamRatio)} ratio={streamRatio} tone="info" />
        <SignalRow label="缓存读取率" value={formatPercent(cacheReadRatio)} ratio={cacheReadRatio} tone="success" />
        <SignalRow
          label="Sticky 回退"
          value={`${formatNumber(fallbackFromStickyRequests)} / sticky ${formatNumber(stickyBoundRequests)}`}
          ratio={stickyBoundRequests > 0 ? fallbackFromStickyRequests / stickyBoundRequests : 0}
          tone={fallbackFromStickyRequests > 0 ? 'warning' : 'default'}
        />
        <SignalRow label="模拟用量" value={formatNumber(simulatedRequests)} tone={simulatedRequests > 0 ? 'warning' : 'default'} />
        <SignalRow label="上游元数据" value={formatNumber(upstreamMetadataRequests)} tone="info" />
      </div>
    </Panel>
  )
}

function ExternalPoolBillingPanel({ billing }: { billing: UsageExternalPoolBillingSummary }) {
  const floorRatio = billing.rawCostUsd > 0 ? billing.costFloorDeltaUsd / billing.rawCostUsd : 0
  const reportedGap = billing.reportedCostUsd - billing.rawCostUsd
  const hasRisk = billing.costFloorDeltaUsd > 0

  return (
    <Panel
      title="备用池成本保护"
      subtitle="按外部池原始 usage 与整形后 usage 分别计价，最终费用不低于可计算渠道成本"
      actions={<Badge variant={hasRisk ? 'warning' : 'success'}>{hasRisk ? `补差 ${formatUsd(billing.costFloorDeltaUsd)}` : '无补差'}</Badge>}
    >
      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">外部池请求</div>
          <div className="mt-1 text-lg font-semibold">{formatNumber(billing.requests)}</div>
          <div className="mt-1 text-xs text-muted-foreground">可计价 {formatNumber(billing.pricedRequests)} / 未计价 {formatNumber(billing.unpricedRequests)}</div>
        </div>
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">渠道原始成本</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(billing.rawCostUsd)}</div>
          <div className="mt-1 text-xs text-muted-foreground">按备用池 raw usage 估算</div>
        </div>
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">整形展示成本</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(billing.reportedCostUsd)}</div>
          <div className={`mt-1 text-xs ${reportedGap < 0 ? 'text-yellow-600 dark:text-yellow-400' : 'text-muted-foreground'}`}>
            对比渠道 {reportedGap >= 0 ? '+' : ''}{formatUsd(reportedGap)}
          </div>
        </div>
        <div className="rounded-md border bg-muted/30 p-3">
          <div className="text-xs text-muted-foreground">最终计费</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(billing.billableCostUsd)}</div>
          <div className="mt-1 text-xs text-muted-foreground">保底触发 {formatNumber(billing.costFloorAppliedRequests)} 次</div>
        </div>
      </div>
      <div className="mt-3">
        <SignalRow
          label="保底补差占渠道成本"
          value={`${formatUsd(billing.costFloorDeltaUsd)} · ${formatPercent(floorRatio)}`}
          ratio={floorRatio}
          tone={hasRisk ? 'warning' : 'success'}
        />
      </div>
    </Panel>
  )
}

export function UsageDashboardPanel() {
  const dashboard = useUsageDashboard(DASHBOARD_TIMEZONE)
  const [selectedWindowKey, setSelectedWindowKey] = useState('today')
  const [rankDimension, setRankDimension] = useState<RankDimension>('credentials')
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
  const top = data.top || EMPTY_TOP
  const series = data.series || { hourly24h: [], daily7d: [] }
  const externalPoolBilling = summary.externalPoolBilling || EMPTY_EXTERNAL_POOL_BILLING
  const pricedRatio = summary.totalRequests > 0 ? summary.pricedRequests / summary.totalRequests : 0
  const streamRatio = summary.totalRequests > 0 ? summary.streamRequests / summary.totalRequests : 0
  const totalTokens = summary.totalInputTokens + summary.totalOutputTokens
  const latencyTone: DashboardTone = summary.p95DurationMs >= 60_000 ? 'warning' : 'default'

  return (
    <div className="space-y-4">
      <DashboardToolbar data={data} selectedWindow={selectedWindow} onWindowChange={setSelectedWindowKey} />

      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-6">
        <MetricCard title="请求健康" value={formatNumber(summary.totalRequests)} desc={`成功 ${formatNumber(summary.successRequests)} / 错误 ${formatNumber(summary.errorRequests)}`} icon={<Activity className="h-5 w-5" />} tone={summary.errorRequests > 0 ? 'warning' : 'info'} />
        <MetricCard title="错误率" value={formatPercent(summary.errorRate)} desc={summary.errorRequests > 0 ? '需要查看异常摘要' : '当前窗口无错误'} icon={summary.errorRequests > 0 ? <ShieldAlert className="h-5 w-5" /> : <CheckCircle2 className="h-5 w-5" />} tone={summary.errorRate > 0 ? 'warning' : 'success'} />
        <MetricCard title="耗时" value={`${Math.round(summary.averageDurationMs)}ms`} desc={`P95 ${formatNumber(summary.p95DurationMs)}ms`} icon={<Clock3 className="h-5 w-5" />} tone={latencyTone} />
        <MetricCard title="估算费用" value={formatUsd(summary.totalEstimatedCostUsd)} desc={`计价覆盖 ${formatPercent(pricedRatio)}`} icon={<DollarSign className="h-5 w-5" />} tone={pricedRatio < 1 && summary.totalRequests > 0 ? 'warning' : 'info'} />
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

      <ExternalPoolBillingPanel billing={externalPoolBilling} />

      <div className="grid gap-3 xl:grid-cols-2">
        <BreakdownPanel title="状态分布" subtitle="判断成功、上游超时、客户端错误等整体占比" items={summary.statusBreakdown || []} emptyText="暂无状态样本。" />
        <BreakdownPanel title="用量来源" subtitle="判断真实上游、缓存模拟、补录等来源占比" items={summary.usageSourceBreakdown || []} emptyText="暂无来源样本。" />
      </div>

      <DimensionRankPanel top={top} activeKey={rankDimension} onActiveKeyChange={setRankDimension} />

      <div className="rounded-lg border bg-card px-3 py-2.5 text-xs text-muted-foreground">
        <div className="flex flex-wrap items-center gap-2">
          <Gauge className="h-4 w-4" />
          <span>总览保留 Top 维度聚合；单条请求链路和更精确筛选请在“Usage”页查看。</span>
          <LineChart className="h-4 w-4" />
          <span>页面数据每 {AUTO_REFRESH_SECONDS} 秒自动刷新。</span>
          {summary.errorRequests > 0 && (
            <>
              <AlertTriangle className="h-4 w-4 text-yellow-600 dark:text-yellow-400" />
              <span className="text-yellow-700 dark:text-yellow-400">当前窗口存在错误请求，优先查看异常摘要和用量详情。</span>
            </>
          )}
          {summary.fallbackFromStickyRequests > 0 && (
            <>
              <Zap className="h-4 w-4 text-yellow-600 dark:text-yellow-400" />
              <span className="text-yellow-700 dark:text-yellow-400">检测到 Sticky 回退，说明粘度命中的账号不可用或并发不可用。</span>
            </>
          )}
        </div>
      </div>
    </div>
  )
}
