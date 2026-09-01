import { useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import {
  Activity,
  CheckCircle2,
  Clock3,
  Database,
  DollarSign,
  ChevronLeft,
  ChevronRight,
  RefreshCw,
  ShieldAlert,
  TrendingUp,
  Users,
  Zap,
} from 'lucide-react'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import {
  useUsageDashboardAccounts,
  useUsageDashboardBreakdown,
  useUsageDashboardExternalPoolBilling,
  useUsageDashboardSeries,
  useUsageDashboardTop,
  useUsageDashboardWindows,
  useUsageSummary,
  useUsageWriterStats,
} from '@/hooks/use-usage'
import { useCredentialSummary } from '@/hooks/use-credentials'
import { formatCompact, formatDate, formatNumber, formatPercent, formatUsdFixed2 } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import { ExternalPoolBillingPanel } from '../usage/usage-billing'
import type {
  UsageBreakdownItem,
  UsageDashboardTop,
  UsageDashboardWindow,
  UsageExternalPoolBillingSummary,
  UsageSeriesPoint,
  UsageTopAggregate,
  UsageDashboardAccountsResponse,
  UsageRecorderStats,
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
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui'
import {
  TrendAreaChart,
  TrendBarChart,
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

type RankDimension = 'models' | 'errors' | 'endpoints' | 'credentials'
type DashboardSection = 'operations' | 'traffic' | 'billing' | 'accounts' | 'errors'
type AccountStatusFilter = 'all' | 'enabled' | 'disabled' | 'active' | 'idle'

const rankDimensions: Array<{ key: RankDimension; label: string }> = [
  { key: 'models', label: '模型' },
  { key: 'errors', label: '错误' },
  { key: 'endpoints', label: '入口' },
  { key: 'credentials', label: '账号' },
]

const accountStatusFilters: Array<{ key: AccountStatusFilter; label: string }> = [
  { key: 'all', label: '全部' },
  { key: 'enabled', label: '启用' },
  { key: 'disabled', label: '禁用' },
  { key: 'active', label: '窗口活跃' },
  { key: 'idle', label: '窗口空闲' },
]

const EMPTY_TOP: UsageDashboardTop = {
  windowKey: '',
  models: [] as UsageTopAggregate[],
  credentials: [] as UsageTopAggregate[],
  endpoints: [] as UsageTopAggregate[],
  errors: [] as UsageTopAggregate[],
  modelsTotal: 0,
  credentialsTotal: 0,
  endpointsTotal: 0,
  errorsTotal: 0,
  modelsTruncated: false,
  credentialsTruncated: false,
  endpointsTruncated: false,
  errorsTruncated: false,
  orderBy: 'estimated_cost_usd',
  errorsOrderBy: 'error_requests',
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
    originalCost: p.totalOriginalCostUsd,
    inputTokens: p.totalInputTokens,
    outputTokens: p.totalOutputTokens,
    kiroMetering: p.totalKiroMeteringUsage ?? 0,
  }
}

function formatDuration(ms: number): string {
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)}m`
  if (ms >= 1_000) return `${(ms / 1_000).toFixed(1)}s`
  return `${Math.round(ms)}ms`
}

function getRankCoverage(top: UsageDashboardTop, key: RankDimension) {
  switch (key) {
    case 'models':
      return {
        total: top.modelsTotal,
        returned: top.models.length,
        truncated: top.modelsTruncated,
      }
    case 'errors':
      return {
        total: top.errorsTotal,
        returned: top.errors.length,
        truncated: top.errorsTruncated,
      }
    case 'endpoints':
      return {
        total: top.endpointsTotal,
        returned: top.endpoints.length,
        truncated: top.endpointsTruncated,
      }
    case 'credentials':
      return {
        total: top.credentialsTotal,
        returned: top.credentials.length,
        truncated: top.credentialsTruncated,
      }
  }
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
        title="最近 24 小时趋势"
        description="按小时聚合；不受当前窗口切换影响"
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
        title="最近 7 天趋势"
        description="按天聚合；不受当前窗口切换影响"
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
        <div className="grid gap-2 text-xs min-w-[160px]">
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
          <div className="flex items-center gap-2 rounded-lg bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
            <CheckCircle2 className="size-4 shrink-0 text-success" />
            当前窗口无错误聚合
          </div>
        ) : (
          visible.map((item, idx) => (
            <div
              key={`${item.key}-${idx}`}
              className="relative overflow-hidden rounded-lg bg-card px-3 py-2.5 shadow-sm"
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
                <span>占全部错误 {formatPercent(totalErrors > 0 ? item.requests / totalErrors : 0)}</span>
                <span className="text-right">估 {formatUsdFixed2(item.totalEstimatedCostUsd)}</span>
                <span className="text-right">原 {formatUsdFixed2(item.totalOriginalCostUsd)}</span>
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

function ScopeNote({
  label,
  text,
}: {
  label: string
  text: string
}) {
  return (
    <div className="inline-flex items-center gap-1 rounded-full bg-muted/50 px-2 py-1 text-[0.68rem] text-muted-foreground">
      <span className="font-semibold text-foreground/70">{label}</span>
      <span>{text}</span>
    </div>
  )
}

function UsageWriterHealthPanel({
  stats,
  error,
}: {
  stats?: UsageRecorderStats
  error?: unknown
}) {
  if (error) {
    return (
      <SectionCard title="统计健康" description="usage writer 与统计持久化状态" icon={<Database />}>
        <ErrorState title="统计健康加载失败" message={extractErrorMessage(error)} />
      </SectionCard>
    )
  }

  if (!stats) {
    return (
      <SectionCard title="统计健康" description="usage writer 与统计持久化状态" icon={<Database />}>
        <LoadingState text="加载统计健康..." className="py-6" />
      </SectionCard>
    )
  }

  const persistCapacity = stats.writerQueueCapacity ?? 0
  const persistAvailable = stats.writerQueueAvailable ?? 0
  const persistUsed = Math.max(0, persistCapacity - persistAvailable)
  const persistRatio = persistCapacity > 0 ? persistUsed / persistCapacity : 0
  const redisCapacity = stats.redisQueueCapacity ?? 0
  const redisAvailable = stats.redisQueueAvailable ?? 0
  const redisUsed = Math.max(0, redisCapacity - redisAvailable)
  const redisRatio = redisCapacity > 0 ? redisUsed / redisCapacity : 0
  const dropped = (stats.droppedPersistRecords ?? 0) + (stats.droppedRedisRecords ?? 0)

  return (
    <SectionCard
      title="统计健康"
      description="观测写入状态；异常只应影响统计，不应阻塞模型请求"
      icon={<Database />}
      actions={
        dropped > 0
          ? <Badge tone="warning" title={formatNumber(dropped)}>已丢弃统计</Badge>
          : <Badge tone="success">无丢弃</Badge>
      }
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <SignalRow
          label="PgSQL writer 队列"
          value={`${formatCompact(persistUsed)} / ${formatCompact(persistCapacity)}`}
          ratio={persistRatio}
          barColor={persistRatio > 0.8 ? 'bg-warning/80' : 'bg-primary/70'}
        />
        <SignalRow
          label="Redis writer 队列"
          value={`${formatCompact(redisUsed)} / ${formatCompact(redisCapacity)}`}
          ratio={redisRatio}
          barColor={redisRatio > 0.8 ? 'bg-warning/80' : 'bg-info/70'}
        />
        <SignalRow
          label="内存保留记录"
          value={`${formatCompact(stats.inMemoryRecords)} / ${formatCompact(stats.inMemoryLimit)}`}
          ratio={stats.inMemoryLimit > 0 ? stats.inMemoryRecords / stats.inMemoryLimit : 0}
        />
        <SignalRow
          label="丢弃统计记录"
          value={formatCompact(dropped)}
          ratio={dropped > 0 ? 1 : 0}
          barColor={dropped > 0 ? 'bg-warning/80' : 'bg-success/70'}
          title="包括 PgSQL/Redis 统计队列满时被保护性丢弃的记录"
        />
      </div>
      <div className="mt-3 flex flex-wrap gap-2 text-[0.68rem] text-muted-foreground">
        <ScopeNote label="PgSQL" text={stats.postgresEnabled ? '启用' : '未启用'} />
        <ScopeNote label="Redis" text={stats.redisEnabled ? '启用' : '未启用'} />
        <ScopeNote label="Redis队列" text={stats.redisQueueEnabled ? '启用' : '未启用'} />
      </div>
    </SectionCard>
  )
}

// ─── 子组件：维度排行 ──────────────────────────────────────────────────────────

function DimensionRankPanel({
  top,
  activeKey,
  onActiveKeyChange,
  totalErrors,
}: {
  top: UsageDashboardTop
  activeKey: RankDimension
  onActiveKeyChange: (k: RankDimension) => void
  totalErrors: number
}) {
  const items = top[activeKey] ?? []
  const isErrorRank = activeKey === 'errors'
  const countLabel = isErrorRank ? '发生次数' : '请求'
  const shareLabel = isErrorRank ? '占全部错误' : '错误率'
  const coverage = getRankCoverage(top, activeKey)
  const coverageText = `Top ${formatNumber(coverage.returned)} / 共 ${formatNumber(coverage.total)}${coverage.truncated ? ' · 已截断' : ''}`
  const description = isErrorRank
    ? `错误维度排行；共 ${formatNumber(totalErrors)} 个错误请求`
    : '维度排行只显示 Top N 结果，不能当作全量统计。'

  return (
    <SectionCard
      title="维度排行"
      description={description}
      icon={<TrendingUp />}
      actions={
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex overflow-hidden rounded-lg bg-muted/40 p-0.5">
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
          <Badge tone={coverage.truncated ? 'warning' : 'success'} title={coverage.truncated ? '排行结果已截断，当前只显示 Top N' : '排行结果完整'}>
            {coverageText}
          </Badge>
        </div>
      }
      noPadding
    >
      {items.length === 0 ? (
        <div className="px-4 py-3 text-xs text-muted-foreground/60">暂无排行数据</div>
      ) : (
        <div className="scrollbar-thin overflow-x-auto">
              <Table className="min-w-[760px]">
            <TableHeader>
              <TableRow>
                <TableHead className="w-8">#</TableHead>
                <TableHead>名称</TableHead>
                <TableHead className="text-right">{countLabel}</TableHead>
                <TableHead className="text-right">{shareLabel}</TableHead>
                <TableHead className="text-right">估算成本</TableHead>
                <TableHead className="text-right">原始计费</TableHead>
                <TableHead className="text-right">Kiro 积分</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((item, idx) => (
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
                  <TableCell className="text-right font-mono text-xs">
                    <span title={formatNumber(item.requests)}>{formatCompact(item.requests)}</span>
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs">
                    {isErrorRank
                      ? formatPercent(totalErrors > 0 ? item.requests / totalErrors : 0)
                      : formatPercent(item.requests > 0 ? item.errorRequests / item.requests : 0)}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs">{formatUsdFixed2(item.totalEstimatedCostUsd)}</TableCell>
                  <TableCell className="text-right font-mono text-xs">{formatUsdFixed2(item.totalOriginalCostUsd)}</TableCell>
                  <TableCell className="text-right font-mono text-xs">{formatCompact(item.totalKiroMeteringUsage ?? 0)}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
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
        <div className="inline-flex overflow-hidden rounded-lg bg-muted/40 p-0.5">
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
          <div className="rounded-lg bg-muted/30 px-3 py-3 text-sm text-muted-foreground/60">
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

// ─── 子组件：轻量实时状态 ──────────────────────────────────────────────────────

function LoadingSkeletonCard() {
  return (
    <div className="min-h-[6.5rem] animate-pulse rounded-xl bg-card p-4 shadow-sm">
      <div className="h-3 w-20 rounded bg-muted" />
      <div className="mt-4 h-7 w-24 rounded bg-muted" />
      <div className="mt-5 h-2 w-full rounded bg-muted" />
      <div className="mt-2 h-2 w-2/3 rounded bg-muted" />
    </div>
  )
}

function DashboardLoadingSkeleton() {
  return (
    <div className="space-y-3">
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        {Array.from({ length: 4 }).map((_, index) => (
          <LoadingSkeletonCard key={index} />
        ))}
      </div>
      <div className="grid gap-3 xl:grid-cols-2">
        <SectionCard title="运行状态" description="正在加载实时状态">
          <LoadingState text="加载实时状态..." className="py-6" />
        </SectionCard>
        <SectionCard title="账号池" description="正在加载账号池状态">
          <LoadingState text="加载账号池..." className="py-6" />
        </SectionCard>
      </div>
    </div>
  )
}

function RealtimeUsagePanel({
  summary,
  error,
}: {
  summary?: ReturnType<typeof useUsageSummary>['data']
  error?: unknown
}) {
  if (error) {
    return (
      <SectionCard title="实时负载" description="最近 60 秒请求、错误、Token 速率" icon={<Activity />}>
        <ErrorState title="实时负载加载失败" message={extractErrorMessage(error)} />
      </SectionCard>
    )
  }

  if (!summary) {
    return (
      <SectionCard title="实时负载" description="最近 60 秒请求、错误、Token 速率" icon={<Activity />}>
        <LoadingState text="加载实时负载..." className="py-6" />
      </SectionCard>
    )
  }

  const realtime = summary.realtime
  const errorRate = realtime.requests > 0 ? (realtime.errorRequests ?? 0) / realtime.requests : 0

  return (
    <SectionCard
      title="实时负载"
      description={`最近 ${realtime.windowSeconds} 秒，判断是否正在被打爆或错误放大`}
      icon={<Activity />}
      actions={
        errorRate > 0.2
          ? <Badge tone="error">错误偏高</Badge>
          : realtime.rpm > 0
            ? <Badge tone="success">有流量</Badge>
            : <Badge tone="secondary">空闲</Badge>
      }
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <SignalRow label="RPM" value={formatNumber(realtime.rpm)} ratio={realtime.rpm > 0 ? 1 : 0} barColor="bg-primary/70" />
        <SignalRow label="错误 RPM" value={formatNumber(realtime.errorRpm ?? 0)} ratio={errorRate} barColor={errorRate > 0 ? 'bg-destructive/75' : 'bg-muted-foreground/30'} />
        <SignalRow label="总 TPM" value={formatNumber(realtime.totalTpm)} ratio={realtime.totalTpm > 0 ? 1 : 0} barColor="bg-info/70" />
        <SignalRow label="计费 TPM" value={formatNumber(realtime.billableTpm)} ratio={realtime.totalTpm > 0 ? realtime.billableTpm / realtime.totalTpm : 0} barColor="bg-success/70" />
      </div>
      <div className="mt-3 rounded-lg bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
        请求 {formatNumber(realtime.requests)} · 成功 {formatNumber(realtime.successRequests ?? 0)} · 错误 {formatNumber(realtime.errorRequests ?? 0)}
      </div>
    </SectionCard>
  )
}

function WindowHealthPanel({
  summary,
}: {
  summary: UsageDashboardWindow['summary']
}) {
  const successRate = summary.totalRequests > 0
    ? summary.successRequests / summary.totalRequests
    : 0
  const latencyTone: 'warning' | 'info' = summary.p95DurationMs >= 60_000 ? 'warning' : 'info'

  return (
    <SectionCard
      title="窗口健康"
      description="当前所选时间窗口；与最近 60 秒实时负载分开统计"
      icon={<CheckCircle2 />}
      actions={
        summary.errorRequests > 0
          ? <Badge tone={summary.errorRate >= 0.2 ? 'error' : 'warning'}>{formatCompact(summary.errorRequests)} 个错误</Badge>
          : <Badge tone="success">无错误</Badge>
      }
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <SignalRow
          label="请求量"
          value={<span title={formatNumber(summary.totalRequests)}>{formatCompact(summary.totalRequests)}</span>}
          ratio={summary.totalRequests > 0 ? 1 : 0}
        />
        <SignalRow
          label="成功率"
          value={formatPercent(successRate)}
          ratio={successRate}
          barColor={successRate >= 0.95 ? 'bg-success/70' : successRate >= 0.8 ? 'bg-warning/80' : 'bg-destructive/75'}
        />
        <SignalRow
          label="P95 耗时"
          value={formatDuration(summary.p95DurationMs)}
          ratio={summary.p95DurationMs > 0 ? Math.min(1, summary.p95DurationMs / 60_000) : 0}
          barColor={latencyTone === 'warning' ? 'bg-warning/80' : 'bg-info/70'}
        />
        <SignalRow
          label="错误请求"
          value={<span title={formatNumber(summary.errorRequests)}>{formatCompact(summary.errorRequests)}</span>}
          ratio={summary.errorRate}
          barColor={summary.errorRequests > 0 ? 'bg-destructive/75' : 'bg-success/70'}
        />
      </div>
    </SectionCard>
  )
}

function AccountStatusFilterBar({
  status,
  onStatusChange,
}: {
  status: AccountStatusFilter
  onStatusChange: (status: AccountStatusFilter) => void
}) {
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <span className="mr-1 text-xs font-medium text-muted-foreground">快捷筛选</span>
      {accountStatusFilters.map((filter) => (
        <Button
          key={filter.key}
          type="button"
          size="xs"
          variant={status === filter.key ? 'default' : 'ghost'}
          onClick={() => onStatusChange(filter.key)}
        >
          {filter.label}
        </Button>
      ))}
    </div>
  )
}

function AccountQualityPanel({
  data,
  loading,
  onPageChange,
  status,
  onStatusChange,
}: {
  data?: UsageDashboardAccountsResponse
  loading: boolean
  onPageChange: (page: number) => void
  status: AccountStatusFilter
  onStatusChange: (status: AccountStatusFilter) => void
}) {
  return (
    <SectionCard
      title="本地账号统计"
      description="全量本地账号按窗口用量分页展示；窗口积分来自请求 meteringEvent，余额快照单独展示；统计只读，不触发余额查询、Token 刷新或调度"
      icon={<Users />}
      actions={
        data
          ? <Badge tone={data.complete ? 'success' : 'warning'}>{data.complete ? '统计完整' : '降级快照'}</Badge>
          : undefined
      }
      noPadding
    >
      {loading ? (
        <div className="p-4">
          <LoadingState text="加载全量账号统计..." className="py-8" />
        </div>
      ) : !data || data.filteredTotal === 0 ? (
        <div className="p-4">
          <AccountStatusFilterBar status={status} onStatusChange={onStatusChange} />
          <EmptyState title="暂无匹配账号" description="当前窗口没有可展示的本地账号。" className="py-8" />
        </div>
      ) : (
        <div>
          {!data.complete && (
            <div className="border-b border-warning/20 bg-warning/10 px-4 py-2 text-xs text-warning">
              账号统计不完整：{data.reason ?? '聚合查询暂时不可用'}。已保留运行态和零值账号。
            </div>
          )}
          <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border/60 px-4 py-3">
            <AccountStatusFilterBar status={status} onStatusChange={onStatusChange} />
            <span className="text-[0.68rem] text-muted-foreground">
              按窗口请求量排序 · 零请求账号保留
            </span>
          </div>
          <div className="scrollbar-thin overflow-x-auto">
            <Table className="min-w-[1240px]">
              <TableHeader>
                <TableRow>
                  <TableHead>账号</TableHead>
                  <TableHead>运行态</TableHead>
                  <TableHead className="text-right">并发</TableHead>
                  <TableHead className="text-right">窗口请求</TableHead>
                  <TableHead className="text-right">窗口积分消耗</TableHead>
                  <TableHead className="text-right">余额快照<br />已用 / 总额</TableHead>
                  <TableHead className="text-right">窗口实际费用<br /><span className="font-normal">原始计费</span></TableHead>
                  <TableHead className="text-right">窗口估算成本</TableHead>
                  <TableHead className="text-right">累计实际费用<br /><span className="font-normal">原始计费</span></TableHead>
                  <TableHead className="text-right">累计估算成本</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {data.items.map((account) => {
                  return (
                    <TableRow key={account.id}>
                      <TableCell>
                        <div className="max-w-[220px] truncate text-xs font-semibold" title={account.email ?? account.label}>
                          #{account.id} {account.label}
                        </div>
                        <div className="mt-0.5 flex gap-1 text-[0.62rem] text-muted-foreground/60">
                          <span>{account.authMethod ?? 'oauth'}</span>
                          <span>·</span>
                          <span>{account.endpoint}</span>
                          {account.subscriptionTitle && <><span>·</span><span>{account.subscriptionTitle}</span></>}
                        </div>
                      </TableCell>
                      <TableCell>
                        <div className="flex flex-wrap gap-1">
                          <Badge tone={account.disabled ? 'secondary' : 'success'}>
                            {account.disabled ? '禁用' : '启用'}
                          </Badge>
                          {account.isCurrent && <Badge tone="info">当前</Badge>}
                          {account.rateLimited && <Badge tone="warning">限流</Badge>}
                          {account.cooledDown && <Badge tone="warning">冷却</Badge>}
                        </div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs" title={`当前在途 ${formatNumber(account.inFlightRequests)}，并发上限 ${account.maxConcurrentRequests > 0 ? formatNumber(account.maxConcurrentRequests) : '不限'}`}>
                        {formatCompact(account.inFlightRequests)}/{account.maxConcurrentRequests > 0 ? formatCompact(account.maxConcurrentRequests) : '∞'}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        <div title={formatNumber(account.windowRequests)}>{formatCompact(account.windowRequests)}</div>
                        <div className={account.windowErrorRequests > 0 ? 'text-[0.62rem] text-warning' : 'text-[0.62rem] text-muted-foreground/60'} title={formatNumber(account.windowErrorRequests)}>
                          错误 {formatCompact(account.windowErrorRequests)}
                        </div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        <div title={`窗口请求对应的 Kiro meteringEvent 消耗；累计 ${formatNumber(account.lifetimeKiroMeteringUsage)}`}>
                          {formatCompact(account.windowKiroMeteringUsage)}
                        </div>
                        <div className="text-[0.62rem] text-muted-foreground/60">
                          累计 {formatCompact(account.lifetimeKiroMeteringUsage)}
                        </div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        {account.creditUsed != null && account.creditLimit != null
                          ? <div title={`已用 ${formatNumber(account.creditUsed)}，剩余 ${formatNumber(account.creditRemaining ?? 0)}，快照 ${account.accountInfoCheckedAt ?? '未知'}`}>
                            {formatCompact(account.creditUsed)} / {formatCompact(account.creditLimit)}
                          </div>
                          : <div className="text-muted-foreground" title="暂无已保存的账号积分快照">-</div>}
                        {account.creditRemaining != null && (
                          <div className="text-[0.62rem] text-muted-foreground/60">
                            剩余 {formatCompact(account.creditRemaining)}
                          </div>
                        )}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        <div title="优先按上游原始 usage 计费；上游没有原始 usage 时后端回退估算成本">
                          {formatUsdFixed2(account.windowOriginalCostUsd)}
                        </div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        {formatUsdFixed2(account.windowEstimatedCostUsd)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        <div title="优先按上游原始 usage 计费；上游没有原始 usage 时后端回退估算成本">
                          {formatUsdFixed2(account.lifetimeOriginalCostUsd)}
                        </div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        {formatUsdFixed2(account.lifetimeEstimatedCostUsd)}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border/60 px-4 py-3 text-xs text-muted-foreground">
            <span>
              显示 {(data.page - 1) * data.pageSize + (data.items.length ? 1 : 0)}{data.items.length ? `-${(data.page - 1) * data.pageSize + data.items.length}` : ''} / {formatNumber(data.filteredTotal)} 个匹配账号 · 窗口活跃 {formatNumber(data.windowActiveLocalAccounts)} · 空闲 {formatNumber(data.windowIdleLocalAccounts)}
            </span>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="xs"
                aria-label="上一页"
                disabled={data.page <= 1}
                onClick={() => onPageChange(data.page - 1)}
              >
                <ChevronLeft className="size-3.5" />
              </Button>
              <span className="font-mono tabular-nums">{data.page} / {Math.max(1, data.totalPages)}</span>
              <Button
                variant="outline"
                size="xs"
                aria-label="下一页"
                disabled={data.page >= data.totalPages}
                onClick={() => onPageChange(data.page + 1)}
              >
                <ChevronRight className="size-3.5" />
              </Button>
            </div>
          </div>
        </div>
      )}
    </SectionCard>
  )
}

// ─── 主页 ──────────────────────────────────────────────────────────────────────

export function OverviewPage() {
  const autoRefresh = useAutoRefreshPreference(OVERVIEW_AUTO_REFRESH_KEY, 30)
  const windowsQuery = useUsageDashboardWindows(OVERVIEW_TIMEZONE, autoRefresh.refetchInterval)
  const [selectedWindowKey, setSelectedWindowKey] = useState('today')
  const [rankDimension, setRankDimension] = useState<RankDimension>('models')
  const [activeSection, setActiveSection] = useState<DashboardSection>('operations')
  const [accountsPage, setAccountsPage] = useState(1)
  const [accountStatus, setAccountStatus] = useState<AccountStatusFilter>('all')

  const data = windowsQuery.data
  const selectedWindow = useMemo(
    () => activeWindow(data?.windows ?? [], selectedWindowKey),
    [data?.windows, selectedWindowKey]
  )
  const effectiveWindowKey = selectedWindow?.key ?? selectedWindowKey
  const usageSummaryQuery = useUsageSummary(autoRefresh.refetchInterval)
  const writerStatsQuery = useUsageWriterStats(autoRefresh.refetchInterval)
  const seriesQuery = useUsageDashboardSeries(
    OVERVIEW_TIMEZONE,
    autoRefresh.refetchInterval,
    activeSection === 'traffic'
  )
  const topQuery = useUsageDashboardTop(
    OVERVIEW_TIMEZONE,
    effectiveWindowKey,
    autoRefresh.refetchInterval,
    activeSection === 'traffic' || activeSection === 'errors'
  )
  const accountsQuery = useUsageDashboardAccounts(
    {
      timezone: OVERVIEW_TIMEZONE,
      windowKey: effectiveWindowKey,
      page: accountsPage,
      pageSize: 50,
      status: accountStatus,
      sortBy: 'window_requests',
      sortOrder: 'desc',
    },
    autoRefresh.refetchInterval,
    activeSection === 'accounts'
  )
  const breakdownQuery = useUsageDashboardBreakdown(
    OVERVIEW_TIMEZONE,
    effectiveWindowKey,
    autoRefresh.refetchInterval,
    activeSection === 'errors'
  )
  const externalPoolBillingQuery = useUsageDashboardExternalPoolBilling(
    OVERVIEW_TIMEZONE,
    effectiveWindowKey,
    autoRefresh.refetchInterval,
    activeSection === 'billing'
  )
  // 加载态
  if (windowsQuery.isLoading) {
    return (
      <PageContainer>
        <PageHeader title="总览" subtitle="实时健康、关键指标与异常" />
        <DashboardLoadingSkeleton />
      </PageContainer>
    )
  }

  // 错误态
  if (windowsQuery.error) {
    return (
      <PageContainer>
        <PageHeader title="总览" subtitle="实时健康、关键指标与异常" />
        <ErrorState title="总览加载失败" message={extractErrorMessage(windowsQuery.error)} />
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
  const top = topQuery.data?.top ?? EMPTY_TOP
  const series = seriesQuery.data?.series ?? { hourly24h: [], daily7d: [] }
  const statusBreakdown = breakdownQuery.data?.statusBreakdown ?? summary.statusBreakdown ?? []
  const usageSourceBreakdown = breakdownQuery.data?.usageSourceBreakdown ?? summary.usageSourceBreakdown ?? []
  const externalPoolBillingByPool =
    externalPoolBillingQuery.data?.externalPoolBillingByPool ??
    summary.externalPoolBillingByPool ??
    []

  const pricedRatio = summary.totalRequests > 0 ? summary.pricedRequests / summary.totalRequests : 0
  const partialErrors = [
    usageSummaryQuery.error ? `实时：${extractErrorMessage(usageSummaryQuery.error)}` : '',
    writerStatsQuery.error ? `统计健康：${extractErrorMessage(writerStatsQuery.error)}` : '',
    seriesQuery.error ? `趋势：${extractErrorMessage(seriesQuery.error)}` : '',
    topQuery.error ? `排行：${extractErrorMessage(topQuery.error)}` : '',
    accountsQuery.error ? `账号统计：${extractErrorMessage(accountsQuery.error)}` : '',
    breakdownQuery.error ? `分布：${extractErrorMessage(breakdownQuery.error)}` : '',
    externalPoolBillingQuery.error ? `外部池计费：${extractErrorMessage(externalPoolBillingQuery.error)}` : '',
  ].filter(Boolean)

  const headerActions = (
    <div className="flex flex-wrap items-center gap-2">
      {/* 时间窗口 */}
      <div className="inline-flex overflow-hidden rounded-lg bg-muted/40 p-0.5">
        {data.windows.map((w) => (
          <Button
            key={w.key}
            variant={w.key === selectedWindow.key ? 'default' : 'ghost'}
            size="sm"
            className="rounded-none"
            onClick={() => {
              setSelectedWindowKey(w.key)
              setAccountsPage(1)
            }}
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
      <ScopeNote label="时间窗口" text="只控制窗口统计，不控制实时/累计/统计健康" />
      {(windowsQuery.isFetching ||
        seriesQuery.isFetching ||
        topQuery.isFetching ||
        breakdownQuery.isFetching ||
        externalPoolBillingQuery.isFetching ||
        accountsQuery.isFetching) && (
        <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />
      )}
    </div>
  )

  return (
    <PageContainer>
      <PageHeader title="总览" subtitle="实时健康、流量、费用、账号质量与异常诊断" actions={headerActions} />

      {partialErrors.length > 0 && (
        <Callout tone="warning">
          部分 dashboard 数据加载失败：{partialErrors.join('；')}
        </Callout>
      )}

      <Tabs value={activeSection} onValueChange={(value) => setActiveSection(value as DashboardSection)}>
        <TabsList className="flex h-auto flex-wrap justify-start">
          <TabsTrigger value="operations">实时</TabsTrigger>
          <TabsTrigger value="traffic">流量</TabsTrigger>
          <TabsTrigger value="billing">费用</TabsTrigger>
          <TabsTrigger value="accounts">账号质量</TabsTrigger>
          <TabsTrigger value="errors">异常诊断</TabsTrigger>
        </TabsList>

        <TabsContent value="operations" className="space-y-3">
          <div className="grid gap-3 xl:grid-cols-2">
            <RealtimeUsagePanel summary={usageSummaryQuery.data} error={usageSummaryQuery.error} />
            <CredentialPoolPanel />
          </div>
          <WindowHealthPanel summary={summary} />
        </TabsContent>

        <TabsContent value="traffic" className="space-y-3">
          {seriesQuery.isLoading ? (
            <SectionCard title="趋势" description="按小时/天聚合">
              <LoadingState text="加载趋势..." className="py-8" />
            </SectionCard>
          ) : (
            <TrendSection hourly={series.hourly24h ?? []} daily={series.daily7d ?? []} />
          )}

          {topQuery.isLoading ? (
            <SectionCard title="维度排行" description="按当前窗口聚合">
              <LoadingState text="加载排行..." className="py-8" />
            </SectionCard>
          ) : (
            <DimensionRankPanel
              top={top}
              activeKey={rankDimension}
              onActiveKeyChange={setRankDimension}
              totalErrors={summary.errorRequests}
            />
          )}
        </TabsContent>

        <TabsContent value="billing" className="space-y-3">
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-5">
            <StatCard title="窗口估算成本" value={formatUsdFixed2(summary.totalEstimatedCostUsd)} desc={`最终 usage × 价格表 · 计价覆盖 ${formatPercent(pricedRatio)}`} icon={<DollarSign />} tone="primary" />
            <StatCard title="窗口原始计费" value={formatUsdFixed2(summary.totalOriginalCostUsd)} desc="优先上游原始 usage；缺失时回退估算" icon={<DollarSign />} tone="warning" />
            <StatCard title="窗口 Kiro 积分消耗" value={formatCompact(summary.totalKiroMeteringUsage ?? 0)} valueTitle={formatNumber(summary.totalKiroMeteringUsage ?? 0)} desc="请求级 meteringEvent，不是余额快照" icon={<DollarSign />} tone="info" />
            <StatCard title="未计价请求" value={formatCompact(summary.unpricedRequests)} valueTitle={formatNumber(summary.unpricedRequests)} desc={`已计价 ${formatCompact(summary.pricedRequests)}`} icon={<DollarSign />} tone={summary.unpricedRequests > 0 ? 'warning' : 'success'} />
            <StatCard title="外部池可计费" value={formatUsdFixed2(summary.externalPoolBilling?.billableCostUsd ?? 0)} desc={`原始成本 ${formatUsdFixed2(summary.externalPoolBilling?.rawCostUsd ?? 0)}`} icon={<DollarSign />} tone="info" />
          </div>
          {externalPoolBillingQuery.isLoading ? (
            <SectionCard title="外部池计费" description="按当前窗口拆分">
              <LoadingState text="加载外部池计费..." className="py-8" />
            </SectionCard>
          ) : (
            <ExternalPoolBillingPanel
              billing={summary.externalPoolBilling ?? EMPTY_EXTERNAL_POOL_BILLING}
              billingByPool={externalPoolBillingByPool}
            />
          )}
        </TabsContent>

        <TabsContent value="accounts" className="space-y-3">
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
            <StatCard title="已配置本地账号" value={formatCompact(accountsQuery.data?.configuredLocalAccounts ?? 0)} desc="纳入统计的全部本地账号" icon={<Users />} tone="primary" />
            <StatCard title="窗口活跃账号" value={formatCompact(accountsQuery.data?.windowActiveLocalAccounts ?? 0)} desc="当前窗口有请求" icon={<Activity />} tone="info" />
            <StatCard title="窗口空闲账号" value={formatCompact(accountsQuery.data?.windowIdleLocalAccounts ?? 0)} desc="已配置但当前窗口无请求" icon={<Clock3 />} tone="default" />
            <StatCard title="当前筛选命中" value={formatCompact(accountsQuery.data?.filteredTotal ?? 0)} desc="表格可分页查看全部" icon={<Users />} tone="info" />
          </div>
          <AccountQualityPanel
            data={accountsQuery.data}
            loading={accountsQuery.isLoading}
            onPageChange={setAccountsPage}
            status={accountStatus}
            onStatusChange={(nextStatus) => {
              setAccountStatus(nextStatus)
              setAccountsPage(1)
            }}
          />
        </TabsContent>

        <TabsContent value="errors" className="space-y-3">
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
            <StatCard title="错误率" value={formatPercent(summary.errorRate)} desc={summary.errorRequests > 0 ? '需查看 Top 错误' : '当前窗口无错误'} icon={summary.errorRequests > 0 ? <ShieldAlert /> : <CheckCircle2 />} tone={summary.errorRate >= 0.2 ? 'error' : summary.errorRate > 0 ? 'warning' : 'success'} />
            <StatCard title="错误请求" value={formatCompact(summary.errorRequests)} valueTitle={formatNumber(summary.errorRequests)} desc={`成功 ${formatCompact(summary.successRequests)}`} icon={<ShieldAlert />} tone={summary.errorRequests > 0 ? 'warning' : 'success'} />
            <StatCard title="Sticky 回退" value={formatCompact(summary.fallbackFromStickyRequests)} valueTitle={formatNumber(summary.fallbackFromStickyRequests)} desc={`绑定 ${formatCompact(summary.stickyBoundRequests)}`} icon={<Zap />} tone={summary.fallbackFromStickyRequests > 0 ? 'warning' : 'success'} />
          </div>
          <div className="grid gap-3 xl:grid-cols-[0.9fr_1.1fr]">
            <ErrorSummaryPanel totalErrors={summary.errorRequests} errorRate={summary.errorRate} items={top.errors ?? []} />
            <BreakdownTabPanel statusItems={statusBreakdown} sourceItems={usageSourceBreakdown} />
          </div>
          <UsageWriterHealthPanel
            stats={writerStatsQuery.data}
            error={writerStatsQuery.error}
          />
        </TabsContent>
      </Tabs>

      {/* 9. 底部状态栏 */}
      <div className="rounded-xl bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
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
