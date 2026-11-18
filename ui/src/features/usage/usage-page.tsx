import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import {
  Activity,
  BarChart3,
  ChevronDown,
  ChevronUp,
  Clock3,
  Database,
  DollarSign,
  Eye,
  RefreshCw,
  Trash2,
  Zap,
} from 'lucide-react'
import { toast } from 'sonner'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import {
  useRefreshUsageQueriesAfterCleanup,
  useUsageCleanupStatus,
  useUsageDashboardSeries,
  useUsageRecordsPage,
  useUsageSummary,
  useUsageWriterStats,
} from '@/hooks/use-usage'
import { useModelPricing, useSyncModelPricing } from '@/hooks/use-models'
import { formatCompact, formatDate, formatNumber, formatPercent, formatUsd, formatUsdFixed2, ratio } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import type { UsageRecord, UsageRecordsPageQuery, UsageSeriesPoint } from '@/types/api'
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'
import { CHART_COLORS, TrendAreaChart, TrendBarChart } from '@/components/charts'
import { statusLabel, statusTone } from './usage-helpers'
import { UsageDetailModal } from './usage-detail-modal'
import { UsageCleanupModal } from './usage-cleanup-modal'

const AUTO_REFRESH_KEY = 'claude-pool-admin:auto-refresh:usage'
const PAGE_SIZE = 50
const STATUS_OPTIONS: Array<[string, string]> = [
  ['_all', '全部状态'],
  ['success', '成功'],
  ['error', '失败'],
  ['stream_error', '流式错误'],
  ['upstream_timeout', '上游超时'],
  ['client_dropped', '客户端断开'],
]

function seriesPointToRow(point: UsageSeriesPoint): Record<string, number | string> {
  return {
    label: point.label,
    requests: point.requests,
    errors: point.errorRequests,
    cost: point.totalEstimatedCostUsd,
    originalCost: point.totalOriginalCostUsd,
    inputTokens: point.totalInputTokens,
    outputTokens: point.totalOutputTokens,
  }
}

/** URL 里的 `since`/`until` 可能是 ISO（总览跳转）或 datetime-local 值，统一转成输入框格式。 */
function toDateTimeLocal(value?: string | null): string {
  if (!value) return ''
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

function toIso(value: string): string | undefined {
  if (!value.trim()) return undefined
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString()
}

// ─── 趋势图区 ─────────────────────────────────────────────────────────────────

function TrendView({ refetchInterval }: { refetchInterval: number | false }) {
  const dashboardSeries = useUsageDashboardSeries('Asia/Shanghai', refetchInterval)
  const series = dashboardSeries.data?.series
  const hourlyData = useMemo(() => (series?.hourly24h ?? []).map(seriesPointToRow), [series?.hourly24h])
  const dailyData = useMemo(() => (series?.daily7d ?? []).map(seriesPointToRow), [series?.daily7d])

  if (dashboardSeries.isLoading) return <LoadingState text="加载趋势数据..." className="py-12" />
  if (dashboardSeries.error) return <ErrorState title="趋势加载失败" message={extractErrorMessage(dashboardSeries.error)} />

  const hourlyRequests = hourlyData.reduce((sum, row) => sum + Number(row.requests), 0)
  const dailyOriginal = dailyData.reduce((sum, row) => sum + Number(row.originalCost), 0)
  return (
    <div className="grid gap-3 xl:grid-cols-2">
      <SectionCard
        title="最近 24 小时（按小时）"
        description="请求量与错误趋势"
        actions={hourlyData.length > 0 ? <Badge tone="neutral" title={formatNumber(hourlyRequests)}>{formatCompact(hourlyRequests)} 请求</Badge> : undefined}
      >
        {hourlyData.length === 0
          ? <EmptyState title="暂无数据" className="py-8" />
          : (
            <TrendAreaChart
              data={hourlyData}
              xKey="label"
              series={[
                { key: 'requests', name: '请求', color: CHART_COLORS[0] },
                { key: 'errors', name: '错误', color: CHART_COLORS[4] },
              ]}
              height={200}
              valueFormatter={(value) => formatNumber(Number(value))}
            />
          )}
      </SectionCard>
      <SectionCard
        title="最近 7 天（按天）"
        description="报告估值与原始估值趋势"
        actions={dailyData.length > 0 ? <Badge tone="neutral">{formatUsdFixed2(dailyOriginal)}</Badge> : undefined}
      >
        {dailyData.length === 0
          ? <EmptyState title="暂无数据" className="py-8" />
          : (
            <TrendBarChart
              data={dailyData}
              xKey="label"
              series={[
                { key: 'cost', name: '报告估值', color: CHART_COLORS[2] },
                { key: 'originalCost', name: '原始估值', color: CHART_COLORS[4] },
              ]}
              height={200}
              valueFormatter={(value, key) => (key === 'cost' || key === 'originalCost' ? formatUsdFixed2(Number(value)) : formatNumber(Number(value)))}
            />
          )}
      </SectionCard>
    </div>
  )
}

// ─── 明细记录 ─────────────────────────────────────────────────────────────────

function RecordsView({ onViewDetail, refetchInterval }: { onViewDetail: (record: UsageRecord) => void; refetchInterval: number | false }) {
  const [searchParams] = useSearchParams()
  const [page, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [status, setStatus] = useState('_all')
  const [model, setModel] = useState('')
  const [account, setAccount] = useState(() => searchParams.get('accountId') ?? '')
  const [endpoint, setEndpoint] = useState(() => searchParams.get('endpoint') ?? '')
  const [since, setSince] = useState(() => toDateTimeLocal(searchParams.get('since')))
  const [until, setUntil] = useState(() => toDateTimeLocal(searchParams.get('until')))

  const searchD = useDebouncedValue(search)
  const modelD = useDebouncedValue(model)
  const accountD = useDebouncedValue(account)
  const endpointD = useDebouncedValue(endpoint)

  const filters = useMemo<UsageRecordsPageQuery>(() => ({
    page,
    limit: PAGE_SIZE,
    q: searchD.trim() || undefined,
    status: status === '_all' ? undefined : (status as UsageRecordsPageQuery['status']),
    model: modelD.trim() || undefined,
    endpoint: endpointD.trim() || undefined,
    accountId: accountD.trim() && Number.isFinite(Number(accountD)) ? Number(accountD) : undefined,
    since: toIso(since),
    until: toIso(until),
  }), [accountD, endpointD, modelD, page, searchD, since, status, until])
  const query = useUsageRecordsPage(filters, refetchInterval)

  const reset = <T,>(setter: (value: T) => void) => (value: T) => {
    setter(value)
    setPage(1)
  }

  if (query.isLoading && !query.data) return <LoadingState text="加载明细..." className="py-12" />
  if (query.error) return <ErrorState title="读取用量失败" message={extractErrorMessage(query.error)} action={<Button onClick={() => query.refetch()}>重试</Button>} />
  const records = query.data?.records ?? []

  return (
    <SectionCard title="明细记录" description="按条件筛选请求级用量">
      <div className="grid gap-3 md:grid-cols-3">
        <Input placeholder="搜索请求 id、账号或模型" value={search} onChange={(event) => reset(setSearch)(event.target.value)} />
        <Input placeholder="模型" value={model} onChange={(event) => reset(setModel)(event.target.value)} />
        <Input type="number" placeholder="账号 id" value={account} onChange={(event) => reset(setAccount)(event.target.value)} />
        <Input placeholder="路由（如 /cc/v1/messages）" value={endpoint} onChange={(event) => reset(setEndpoint)(event.target.value)} />
        <Select value={status} onValueChange={reset(setStatus)}>
          <SelectTrigger><SelectValue /></SelectTrigger>
          <SelectContent>
            {STATUS_OPTIONS.map(([value, label]) => <SelectItem key={value} value={value}>{label}</SelectItem>)}
          </SelectContent>
        </Select>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Input type="datetime-local" value={since} onChange={(event) => reset(setSince)(event.target.value)} className="text-xs" />
          <span>至</span>
          <Input type="datetime-local" value={until} onChange={(event) => reset(setUntil)(event.target.value)} className="text-xs" />
        </div>
      </div>
      {records.length === 0 ? (
        <EmptyState title="暂无请求记录" className="py-10" />
      ) : (
        <div className="mt-4 scrollbar-thin overflow-x-auto">
          <Table className="min-w-[1080px] text-xs">
            <TableHeader>
              <TableRow>
                {['时间', '账号', '模型', '路由', '状态', '输入', '输出', '报告估值', '原始估值', '账号成本', '详情'].map((title) => (
                  <TableHead key={title} className={['输入', '输出', '报告估值', '原始估值', '账号成本'].includes(title) ? 'text-right' : undefined}>{title}</TableHead>
                ))}
              </TableRow>
            </TableHeader>
            <TableBody>
              {records.map((record) => (
                <TableRow key={record.id}>
                  <TableCell className="whitespace-nowrap">{formatDate(record.createdAt)}</TableCell>
                  <TableCell>{record.accountName ?? (record.accountId != null ? `#${record.accountId}` : '—')}</TableCell>
                  <TableCell className="max-w-[220px] truncate" title={record.model}>{record.model}</TableCell>
                  <TableCell className="font-mono">{record.endpoint}</TableCell>
                  <TableCell><Badge tone={statusTone(record.status)} size="xs">{statusLabel(record.status)}</Badge></TableCell>
                  <TableCell className="text-right font-mono" title={formatNumber(record.totalInputTokens)}>{formatCompact(record.totalInputTokens)}</TableCell>
                  <TableCell className="text-right font-mono" title={formatNumber(record.outputTokens)}>{formatCompact(record.outputTokens)}</TableCell>
                  <TableCell className="text-right font-mono">{formatUsd(record.estimatedCostUsd)}</TableCell>
                  <TableCell className="text-right font-mono">{formatUsd(record.originalCostUsd)}</TableCell>
                  <TableCell className="text-right font-mono">{formatUsd(record.accountCostUsd)}</TableCell>
                  <TableCell>
                    <Button variant="ghost" size="icon-xs" title="查看详情" onClick={() => onViewDetail(record)}><Eye /></Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      <div className="mt-3 flex items-center justify-end gap-4">
        <Button variant="outline" size="sm" disabled={page === 1} onClick={() => setPage(page - 1)}>上一页</Button>
        <span className="text-xs text-muted-foreground">第 {page} 页</span>
        <Button variant="outline" size="sm" disabled={records.length < PAGE_SIZE} onClick={() => setPage(page + 1)}>下一页</Button>
      </div>
    </SectionCard>
  )
}

// ─── 页面 ─────────────────────────────────────────────────────────────────────

export function UsagePage() {
  const [trendOpen, setTrendOpen] = useState(false)
  const [selected, setSelected] = useState<UsageRecord | null>(null)
  const [cleanupOpen, setCleanupOpen] = useState(false)

  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)
  const summary = useUsageSummary(autoRefresh.refetchInterval)
  const cleanupStatus = useUsageCleanupStatus()
  const modelPricing = useModelPricing(autoRefresh.refetchInterval || 60000)
  const writerStats = useUsageWriterStats(autoRefresh.refetchInterval)
  const syncPricing = useSyncModelPricing()
  useRefreshUsageQueriesAfterCleanup(cleanupStatus.data)

  const data = summary.data
  const totalTokens = (data?.totalInputTokens ?? 0) + (data?.totalOutputTokens ?? 0)
  const errorRate = data && data.totalRequests > 0 ? data.errorRequests / data.totalRequests : 0
  const realtime = data?.realtime
  const realtimeWindow = realtime?.windowSeconds ?? 60
  // 缓存读 / (缓存读 + 缓存写 + 未缓存输入)，与总览口径一致；UsageSummary 的 totalInputTokens 已含缓存 token。
  const cacheRead = data?.totalCacheReadInputTokens ?? 0
  const cacheWrite = data?.totalCacheCreationInputTokens ?? 0
  const cacheHit = ratio(cacheRead, data?.totalInputTokens ?? 0)
  const cachedShare = ratio(cacheRead + cacheWrite, data?.totalInputTokens ?? 0)
  const billing = data?.accountBilling
  const pricingStatus = modelPricing.data
  // 写入失败或排队已满而丢弃的记录不在本页统计里，明确标出数量而不是让页面看起来完整。
  const droppedRecords = (writerStats.data?.droppedPersistRecords ?? 0) + (writerStats.data?.droppedRedisRecords ?? 0)

  const handleSyncPricing = () => {
    syncPricing.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) {
          toast.warning(`价格同步失败，继续使用${status.source === 'built-in' ? '内置价格' : '当前价格'}: ${status.lastError}`)
          return
        }
        toast.success(`价格已同步：${formatNumber(status.modelCount)} 个模型`)
        void summary.refetch()
      },
      onError: (error) => toast.error(`同步失败: ${extractErrorMessage(error)}`),
    })
  }

  const headerActions = (
    <div className="flex flex-wrap items-center gap-2">
      <label className="flex cursor-pointer select-none items-center gap-2 text-xs text-muted-foreground">
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
          onChange={(event) => autoRefresh.setIntervalSeconds(Number(event.target.value))}
        />
        <span className="text-xs text-muted-foreground">秒</span>
      </div>
      <div className="hidden items-center gap-1 rounded-lg border bg-card px-2 py-1 text-xs text-muted-foreground md:flex">
        <DollarSign className="size-3.5" />
        <span className="font-medium text-foreground">计价</span>
        <span>{pricingStatus?.source || 'loading'}</span>
        <span>· {formatNumber(pricingStatus?.modelCount ?? 0)} 模型</span>
        {pricingStatus?.lastError && <span className="max-w-40 truncate text-destructive" title={pricingStatus.lastError}>· {pricingStatus.lastError}</span>}
      </div>
      {droppedRecords > 0 && (
        <div
          className="flex items-center gap-1 rounded-lg border bg-card px-2 py-1 text-xs text-muted-foreground"
          title="写入数据库失败或排队已满而未保存的用量记录数，从本进程启动起累计；这部分请求不在本页统计里。"
        >
          <span className="font-medium text-foreground">未保存记录</span>
          <span className="text-destructive">{formatNumber(droppedRecords)} 条</span>
        </div>
      )}
      <Button variant="outline" size="sm" onClick={handleSyncPricing} disabled={syncPricing.isPending}>
        {syncPricing.isPending ? <Spinner size="sm" /> : <DollarSign className="size-3.5" />}
        同步价格
      </Button>
      {summary.isFetching && <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />}
      <Button variant="outline" size="sm" className="text-destructive hover:bg-destructive/10" onClick={() => setCleanupOpen(true)}>
        <Trash2 className="size-3.5" />清理记录
      </Button>
    </div>
  )

  return (
    <PageContainer>
      <PageHeader title="用量" subtitle="请求趋势与明细记录" actions={headerActions} />

      {summary.error && <Callout tone="error">用量汇总加载失败：{extractErrorMessage(summary.error)}</Callout>}

      <div className="grid gap-3 grid-cols-2 lg:grid-cols-4">
        <StatCard
          title="总请求"
          value={formatCompact(data?.totalRequests ?? 0)}
          valueTitle={formatNumber(data?.totalRequests ?? 0)}
          desc={(
            <span>
              成功 {formatCompact(data?.successRequests ?? 0)} · 错误率{' '}
              <span className={errorRate > 0 ? 'font-semibold text-destructive' : 'font-semibold text-success'}>{formatPercent(errorRate)}</span>
              {' '}({formatCompact(data?.errorRequests ?? 0)})
            </span>
          )}
          icon={<Activity />}
          tone="primary"
        />
        <StatCard
          title="实时 RPM"
          value={formatNumber(realtime?.rpm ?? 0)}
          desc={`近 ${realtimeWindow} 秒 · 成功 ${formatNumber(realtime?.successRequests ?? 0)} · 错误 ${formatNumber(realtime?.errorRequests ?? 0)} · TPM ${formatCompact(realtime?.totalTpm ?? 0)}`}
          icon={<Zap />}
          tone="info"
        />
        <StatCard
          title="Token 用量"
          value={formatCompact(totalTokens)}
          valueTitle={formatNumber(totalTokens)}
          desc={`输入 ${formatCompact(data?.totalInputTokens ?? 0)} / 输出 ${formatCompact(data?.totalOutputTokens ?? 0)}`}
          icon={<Database />}
          tone="info"
        />
        <StatCard
          title="缓存命中率"
          value={Number.isFinite(cacheHit) ? formatPercent(cacheHit) : '-'}
          desc={`缓存读 ${formatCompact(cacheRead)} · 读+写占输入 ${Number.isFinite(cachedShare) ? formatPercent(cachedShare) : '-'} · 高命中 ${formatCompact(data?.highCacheRequests ?? 0)} 次`}
          icon={<Zap />}
          tone="success"
        />
        <StatCard
          title="报告估值"
          value={formatUsdFixed2(data?.totalEstimatedCostUsd ?? 0)}
          desc={`按最终上报 usage 计价 · 计价覆盖 ${formatPercent(ratio(data?.pricedRequests ?? 0, data?.totalRequests ?? 1))}`}
          icon={<Clock3 />}
          tone="primary"
        />
        <StatCard
          title="原始估值"
          value={formatUsdFixed2(data?.totalOriginalCostUsd ?? 0)}
          desc="按上游原始 usage 计价"
          icon={<Clock3 />}
          tone="warning"
        />
        <StatCard
          title="上游原始成本"
          value={formatUsdFixed2(billing?.rawCostUsd ?? 0)}
          desc={`展示计费 ${formatUsdFixed2(billing?.shapedCostUsd ?? 0)} · 原始/整形 ${billing && billing.rawCostUsd > 0 && (billing.shapedCostUsd ?? 0) > 0 ? (billing.rawCostUsd / (billing.shapedCostUsd ?? 1)).toFixed(3) : '-'}`}
          icon={<DollarSign />}
          tone="warning"
        />
        <StatCard
          title="可计费"
          value={formatUsdFixed2(billing?.billableCostUsd ?? 0)}
          desc={(
            <span>
              补偿后 {formatUsdFixed2(billing?.upliftedCostUsd ?? 0)} · 毛利{' '}
              <span className={cn('font-semibold', (billing?.profitUsd ?? 0) < 0 ? 'text-destructive' : 'text-success')}>
                {(billing?.profitUsd ?? 0) >= 0 ? '+' : '-'}{formatUsdFixed2(Math.abs(billing?.profitUsd ?? 0))}
              </span>
              {(billing?.costFloorAppliedRequests ?? 0) > 0 && ` · 兜底 ${formatNumber(billing?.costFloorAppliedRequests ?? 0)} 次`}
            </span>
          )}
          icon={<DollarSign />}
          tone={(billing?.profitUsd ?? 0) < 0 ? 'error' : 'success'}
        />
      </div>

      {data && data.errorRequests > 0 && errorRate >= 0.05 && (
        <Callout tone={errorRate >= 0.2 ? 'error' : 'warning'}>
          当前错误率 {formatPercent(errorRate)}，共 {formatNumber(data.errorRequests)} 次错误请求，建议查看明细。
        </Callout>
      )}

      <div className="rounded-xl bg-card shadow-sm">
        <Button
          variant="ghost"
          className="flex h-auto w-full items-center justify-between rounded-xl px-4 py-3 text-sm font-medium text-foreground/80 hover:bg-muted/40"
          onClick={() => setTrendOpen((value) => !value)}
        >
          <div className="flex items-center gap-2">
            <BarChart3 className="size-4 text-muted-foreground" />
            趋势图
          </div>
          {trendOpen ? <ChevronUp className="size-4 text-muted-foreground" /> : <ChevronDown className="size-4 text-muted-foreground" />}
        </Button>
        {trendOpen && (
          <div className="px-4 pb-4 pt-2">
            <TrendView refetchInterval={autoRefresh.refetchInterval} />
          </div>
        )}
      </div>

      <RecordsView onViewDetail={setSelected} refetchInterval={autoRefresh.refetchInterval} />

      <div className="rounded-xl bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
        <span>用量 · </span>
        <span>{autoRefresh.enabled ? `每 ${autoRefresh.intervalSeconds} 秒自动刷新` : '自动刷新已关闭'}</span>
        {cleanupStatus.data?.status === 'running' && <span className="ml-2 text-warning">· 清理任务执行中...</span>}
      </div>

      <UsageDetailModal record={selected} open={selected !== null} onClose={() => setSelected(null)} />
      <UsageCleanupModal open={cleanupOpen} onClose={() => { setCleanupOpen(false); void summary.refetch() }} />
    </PageContainer>
  )
}
