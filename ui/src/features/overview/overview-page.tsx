import { useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Clock3,
  Database,
  RefreshCw,
  Server,
  ShieldAlert,
  TrendingUp,
  Users,
  Zap,
} from 'lucide-react'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useUsageDashboard } from '@/hooks/use-usage'
import { useCredentialSummary } from '@/hooks/use-credentials'
import { formatNumber, formatPercent, formatUsd } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import type {
  UsageDashboardWindow,
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
            ? <Badge tone="error">错误 {formatNumber(hourlyErrors)}</Badge>
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
            ? <Badge tone="error">错误 {formatNumber(dailyErrors)}</Badge>
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
          label={<span className="text-[0.65rem] font-bold">{Math.round(availRatio * 100)}%</span>}
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
          ? <Badge tone={isHigh ? 'error' : 'warning'}>{formatNumber(totalErrors)} 错误</Badge>
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
                <Badge tone="error">{formatNumber(item.requests)}</Badge>
              </div>
              <div className="mt-1.5 grid grid-cols-3 gap-1 pl-1.5 text-[0.62rem] text-muted-foreground/60">
                <span>输入 {formatNumber(item.totalInputTokens)}</span>
                <span>输出 {formatNumber(item.totalOutputTokens)}</span>
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
}: {
  label: string
  value: ReactNode
  ratio?: number
  barColor?: string
}) {
  const width = Number.isFinite(ratio ?? NaN)
    ? Math.min(100, Math.max(0, (ratio as number) * 100))
    : 0
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="truncate font-medium text-foreground/75">{label}</span>
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
          value={`${formatNumber(stickyFallback)} / ${formatNumber(stickyBound)}`}
          ratio={stickyBound > 0 ? stickyFallback / stickyBound : 0}
          barColor={stickyFallback > 0 ? 'bg-warning/80' : 'bg-muted-foreground/30'}
        />
        <SignalRow
          label="模拟展示"
          value={formatNumber(simulated)}
          barColor={simulated > 0 ? 'bg-info/70' : 'bg-muted-foreground/30'}
        />
        <SignalRow
          label="服务返回用量"
          value={formatNumber(upstreamMeta)}
          barColor="bg-primary/65"
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
                  <TableCell className="text-right font-mono text-xs">{formatNumber(item.requests)}</TableCell>
                  <TableCell className="text-right font-mono text-xs">
                    {item.errorRequests > 0
                      ? <span className="text-destructive">{formatNumber(item.errorRequests)}</span>
                      : <span className="text-muted-foreground/40">—</span>}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs">{formatNumber(item.totalInputTokens)}</TableCell>
                  <TableCell className="text-right font-mono text-xs">{formatNumber(item.totalOutputTokens)}</TableCell>
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
              <div className="mt-1 text-2xl font-semibold tracking-tight tabular-nums text-primary">
                {formatNumber(summary.totalRequests)}
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
              成功 {formatNumber(summary.successRequests)} / 错误 {formatNumber(summary.errorRequests)}
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
          value={formatNumber(totalTokens)}
          desc={`输入 ${formatNumber(summary.totalInputTokens)} / 输出 ${formatNumber(summary.totalOutputTokens)}`}
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
          <div className="mt-1 pl-2.5 truncate text-[0.72rem] text-muted-foreground">
            读取 {formatNumber(summary.totalCacheReadInputTokens)} tokens
          </div>
        </div>

        {/* 估算费用 */}
        <StatCard
          title="估算费用"
          value={formatUsd(summary.totalEstimatedCostUsd)}
          desc={`计价覆盖 ${formatPercent(pricedRatio)}`}
          icon={<Server />}
          tone="primary"
        />
      </div>

      {/* 2. 账号池状态 */}
      <CredentialPoolPanel />

      {/* 3. 异常警示 Callout（仅在有错误时显示在显眼位置） */}
      {summary.errorRequests > 0 && summary.errorRate >= 0.05 && (
        <Callout tone={summary.errorRate >= 0.2 ? 'error' : 'warning'}>
          <div className="flex items-center gap-2">
            <AlertTriangle className="size-3.5 shrink-0" />
            当前窗口错误率 {formatPercent(summary.errorRate)}（{formatNumber(summary.errorRequests)} 次），建议查看下方异常摘要。
          </div>
        </Callout>
      )}

      {summary.fallbackFromStickyRequests > 0 && (
        <Callout tone="warning">
          <div className="flex items-center gap-2">
            <Zap className="size-3.5 shrink-0" />
            检测到 {formatNumber(summary.fallbackFromStickyRequests)} 次 Sticky 回退，说明粘度命中的账号不可用或并发已满。
          </div>
        </Callout>
      )}

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

      {/* 7. 底部状态栏 */}
      <div className="rounded-xl border border-border bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
        <div className="flex flex-wrap items-center gap-2">
          <span>总览 · {selectedWindow.label}</span>
          <span className="text-muted-foreground/40">·</span>
          <span>
            {autoRefresh.enabled
              ? `每 ${autoRefresh.intervalSeconds} 秒自动刷新`
              : '自动刷新已关闭'}
          </span>
          {data.generatedAt && (
            <>
              <span className="text-muted-foreground/40">·</span>
              <span>数据生成于 {new Date(data.generatedAt).toLocaleTimeString('zh-CN')}</span>
            </>
          )}
        </div>
      </div>
    </PageContainer>
  )
}
