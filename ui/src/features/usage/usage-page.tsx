import { useMemo, useState } from 'react'
import {
  Activity,
  BarChart3,
  Clock3,
  Database,
  Filter,
  RefreshCw,
  Trash2,
  X,
  Zap,
} from 'lucide-react'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import {
  useUsageDashboard,
  useUsageSummary,
  useUsageRecordsPage,
  useUsageCleanupStatus,
  useRefreshUsageQueriesAfterCleanup,
} from '@/hooks/use-usage'
import { formatDate, formatNumber, formatPercent, formatUsd } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type { UsageRecord, UsageRecordStatus, UsageSource, UsageSeriesPoint } from '@/types/api'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  EmptyState,
  LoadingState,
  ErrorState,
  Callout,
  Toolbar,
  ToolbarSearch,
  ToolbarActions,
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
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'
import { TrendAreaChart, TrendBarChart, CHART_COLORS } from '@/components/charts'
import { statusLabel, statusTone, routeLabel, routeTone, formatLatency } from './usage-helpers'
import { UsageDetailModal } from './usage-detail-modal'
import { UsageCleanupModal } from './usage-cleanup-modal'

// ─── 常量 ─────────────────────────────────────────────────────────────────────

const AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:usage'
const PAGE_SIZE = 20

type ViewTab = 'trend' | 'records'

// ─── 工具函数 ──────────────────────────────────────────────────────────────────

function seriesPointToRow(p: UsageSeriesPoint): Record<string, number | string> {
  return {
    label: p.label,
    requests: p.requests,
    errors: p.errorRequests,
    cost: p.totalEstimatedCostUsd,
    inputTokens: p.totalInputTokens,
    outputTokens: p.totalOutputTokens,
  }
}

// ─── 趋势图区 ─────────────────────────────────────────────────────────────────

function TrendView() {
  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)
  const dashboard = useUsageDashboard('Asia/Shanghai', autoRefresh.refetchInterval)
  const data = dashboard.data
  const series = data?.series

  const hourlyData = useMemo(() => (series?.hourly24h ?? []).map(seriesPointToRow), [series?.hourly24h])
  const dailyData = useMemo(() => (series?.daily7d ?? []).map(seriesPointToRow), [series?.daily7d])

  if (dashboard.isLoading) return <LoadingState text="加载趋势数据..." className="py-12" />
  if (dashboard.error) return <ErrorState title="趋势加载失败" message={extractErrorMessage(dashboard.error)} />

  return (
    <div className="space-y-3">
      <div className="grid gap-3 xl:grid-cols-2">
        <SectionCard
          title="最近 24 小时（按小时）"
          description="请求量与错误趋势"
          actions={
            hourlyData.length > 0
              ? <Badge tone="neutral">{formatNumber(hourlyData.reduce((s, r) => s + Number(r.requests), 0))} 请求</Badge>
              : undefined
          }
        >
          {hourlyData.length === 0
            ? <EmptyState title="暂无数据" className="py-8" />
            : <TrendAreaChart
                data={hourlyData}
                xKey="label"
                series={[
                  { key: 'requests', name: '请求', color: CHART_COLORS[0] },
                  { key: 'errors', name: '错误', color: CHART_COLORS[4] },
                ]}
                height={200}
                valueFormatter={(v) => formatNumber(Number(v))}
              />
          }
        </SectionCard>

        <SectionCard
          title="最近 7 天（按天）"
          description="估算费用趋势"
          actions={
            dailyData.length > 0
              ? <Badge tone="neutral">{formatUsd(dailyData.reduce((s, r) => s + Number(r.cost), 0))}</Badge>
              : undefined
          }
        >
          {dailyData.length === 0
            ? <EmptyState title="暂无数据" className="py-8" />
            : <TrendBarChart
                data={dailyData}
                xKey="label"
                series={[
                  { key: 'requests', name: '请求', color: CHART_COLORS[0] },
                  { key: 'cost', name: '费用(USD)', color: CHART_COLORS[2] },
                ]}
                height={200}
                valueFormatter={(v, key) =>
                  key === 'cost' ? formatUsd(Number(v)) : formatNumber(Number(v))
                }
              />
          }
        </SectionCard>
      </div>
    </div>
  )
}

// ─── 明细记录表 ───────────────────────────────────────────────────────────────

function RecordsView({
  onViewDetail,
}: {
  onViewDetail: (r: UsageRecord) => void
}) {
  const [page, setPage] = useState(1)
  const [q, setQ] = useState('')
  const [model, setModel] = useState('')
  const [status, setStatus] = useState<UsageRecordStatus | '__all__'>('__all__')
  const [source, setSource] = useState<UsageSource | '__all__'>('__all__')
  const [showFilters, setShowFilters] = useState(false)

  const query = useMemo(() => ({
    page,
    limit: PAGE_SIZE,
    q: q.trim() || undefined,
    model: model.trim() || undefined,
    status: status !== '__all__' ? status : undefined,
    source: source !== '__all__' ? source : undefined,
  }), [page, q, model, status, source])

  const records = useUsageRecordsPage(query)
  const items = records.data?.records ?? []
  const hasNext = records.data?.hasNext ?? false
  const hasFilters = status !== '__all__' || source !== '__all__' || !!q.trim() || !!model.trim()
  const filterCount = [status !== '__all__', source !== '__all__', !!q.trim(), !!model.trim()].filter(Boolean).length

  const clearFilters = () => { setQ(''); setModel(''); setStatus('__all__'); setSource('__all__') }

  return (
    <div className="space-y-3">
      <SectionCard
        title="明细记录"
        description="每次请求的完整记录，点击行查看详情"
        noPadding
      >
        <div className="px-4 pt-4 pb-2">
          <Toolbar>
            <ToolbarSearch value={q} onChange={(v) => { setQ(v); setPage(1) }} placeholder="搜索模型、端点、会话ID..." />
            <ToolbarActions>
              <Button
                variant="outline"
                size="sm"
                className={hasFilters ? 'border-primary text-primary' : ''}
                onClick={() => setShowFilters((v) => !v)}
              >
                <Filter className="h-3.5 w-3.5" />
                筛选
                {filterCount > 0 && <Badge tone="primary">{filterCount}</Badge>}
              </Button>
              {records.isFetching && <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />}
            </ToolbarActions>
          </Toolbar>

          {showFilters && (
            <div className="mt-2 rounded-lg border border-border bg-muted/40 p-3">
              <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
                <Input
                  placeholder="模型"
                  value={model}
                  onChange={(e) => { setModel(e.target.value); setPage(1) }}
                  className="h-8 text-xs"
                />
                <Select value={status} onValueChange={(v) => { setStatus(v as UsageRecordStatus | '__all__'); setPage(1) }}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__all__">全部状态</SelectItem>
                    <SelectItem value="success">成功</SelectItem>
                    <SelectItem value="error">错误</SelectItem>
                    <SelectItem value="stream_error">流错误</SelectItem>
                    <SelectItem value="upstream_timeout">服务超时</SelectItem>
                    <SelectItem value="client_dropped">客户端断开</SelectItem>
                  </SelectContent>
                </Select>
                <Select value={source} onValueChange={(v) => { setSource(v as UsageSource | '__all__'); setPage(1) }}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__all__">全部来源</SelectItem>
                    <SelectItem value="upstream_metadata">服务返回用量</SelectItem>
                    <SelectItem value="local_prompt_cache">本地缓存估算</SelectItem>
                    <SelectItem value="context_estimate">上下文估算</SelectItem>
                    <SelectItem value="request_estimate">请求估算</SelectItem>
                    <SelectItem value="none">无缓存</SelectItem>
                  </SelectContent>
                </Select>
                {hasFilters && (
                  <Button variant="ghost" size="sm" onClick={clearFilters}>
                    <X className="h-3.5 w-3.5" />清除筛选
                  </Button>
                )}
              </div>
            </div>
          )}
        </div>

        {records.isLoading ? (
          <LoadingState text="加载记录..." className="py-8" />
        ) : records.error ? (
          <div className="px-4 pb-4">
            <ErrorState title="记录加载失败" message={extractErrorMessage(records.error)} />
          </div>
        ) : items.length === 0 ? (
          <div className="px-4 pb-4">
            <EmptyState
              title="暂无记录"
              description={hasFilters ? '没有匹配当前筛选条件的记录' : '还没有请求记录'}
              action={hasFilters ? <Button variant="outline" size="sm" onClick={clearFilters}>清除筛选</Button> : undefined}
            />
          </div>
        ) : (
          <>
            <div className="scrollbar-thin overflow-x-auto">
              <Table className="min-w-[860px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>时间</TableHead>
                    <TableHead>模型</TableHead>
                    <TableHead>状态</TableHead>
                    <TableHead>路由</TableHead>
                    <TableHead className="text-right">输入</TableHead>
                    <TableHead className="text-right">输出</TableHead>
                    <TableHead className="text-right">费用</TableHead>
                    <TableHead className="text-right">耗时</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {items.map((record) => (
                    <TableRow
                      key={record.id}
                      className="cursor-pointer"
                      onClick={() => onViewDetail(record)}
                    >
                      <TableCell className="tabular-nums text-muted-foreground text-xs">
                        {formatDate(record.createdAt)}
                      </TableCell>
                      <TableCell>
                        <div className="max-w-[160px] truncate text-xs font-medium" title={record.model}>
                          {record.model}
                        </div>
                        {record.upstreamModel && record.upstreamModel !== record.model && (
                          <div className="truncate font-mono text-[0.62rem] text-muted-foreground/60">
                            {record.upstreamModel}
                          </div>
                        )}
                      </TableCell>
                      <TableCell>
                        <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
                      </TableCell>
                      <TableCell>
                        <Badge tone={routeTone(record)}>{routeLabel(record)}</Badge>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatNumber(record.totalInputTokens)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatNumber(record.outputTokens)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {record.pricingAvailable ? formatUsd(record.estimatedCostUsd) : <span className="text-muted-foreground/40">—</span>}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatLatency(record.durationMs)}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
            {(page > 1 || hasNext) && (
              <div className="border-t border-border px-4 py-3">
                <div className="flex items-center justify-center gap-3">
                  <Button variant="outline" size="sm" disabled={page === 1} onClick={() => setPage((v) => Math.max(1, v - 1))}>上一页</Button>
                  <span className="text-xs text-muted-foreground">第 {page} 页，每页 {PAGE_SIZE} 条</span>
                  <Button variant="outline" size="sm" disabled={!hasNext} onClick={() => setPage((v) => v + 1)}>下一页</Button>
                </div>
              </div>
            )}
          </>
        )}
      </SectionCard>
    </div>
  )
}

// ─── 主页 ──────────────────────────────────────────────────────────────────────

export function UsagePage() {
  const [activeTab, setActiveTab] = useState<ViewTab>('trend')
  const [selectedRecord, setSelectedRecord] = useState<UsageRecord | null>(null)
  const [cleanupOpen, setCleanupOpen] = useState(false)

  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)
  const summary = useUsageSummary(autoRefresh.refetchInterval)
  const cleanupStatus = useUsageCleanupStatus()
  useRefreshUsageQueriesAfterCleanup(cleanupStatus.data)

  const data = summary.data
  const totalTokens = (data?.totalInputTokens ?? 0) + (data?.totalOutputTokens ?? 0)
  const errorRate = data && data.totalRequests > 0 ? data.errorRequests / data.totalRequests : 0

  const headerActions = (
    <div className="flex flex-wrap items-center gap-2">
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
      {summary.isFetching && <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />}
      <Button variant="outline" size="sm" className="text-destructive hover:bg-destructive/10" onClick={() => setCleanupOpen(true)}>
        <Trash2 className="h-3.5 w-3.5" />清理记录
      </Button>
    </div>
  )

  return (
    <PageContainer>
      <PageHeader title="用量" subtitle="请求趋势与明细记录" actions={headerActions} />

      {/* 指标卡 */}
      <StatGrid>
        <StatCard
          title="总请求"
          value={formatNumber(data?.totalRequests ?? 0)}
          desc={`成功 ${formatNumber(data?.successRequests ?? 0)} / 错误 ${formatNumber(data?.errorRequests ?? 0)}`}
          icon={<Activity />}
          tone="primary"
        />
        <StatCard
          title="错误率"
          value={formatPercent(errorRate)}
          desc={errorRate >= 0.2 ? '偏高，请排查' : errorRate > 0 ? '有少量错误' : '当前无错误'}
          icon={<BarChart3 />}
          tone={errorRate >= 0.2 ? 'error' : errorRate > 0 ? 'warning' : 'success'}
        />
        <StatCard
          title="Token 用量"
          value={formatNumber(totalTokens)}
          desc={`输入 ${formatNumber(data?.totalInputTokens ?? 0)} / 输出 ${formatNumber(data?.totalOutputTokens ?? 0)}`}
          icon={<Database />}
          tone="info"
        />
        <StatCard
          title="缓存读取"
          value={formatNumber(data?.totalCacheReadInputTokens ?? 0)}
          desc="Token"
          icon={<Zap />}
          tone="success"
        />
        <StatCard
          title="估算费用"
          value={formatUsd(data?.totalEstimatedCostUsd ?? 0)}
          desc={`计价覆盖 ${formatNumber(data?.pricedRequests ?? 0)} / ${formatNumber(data?.totalRequests ?? 0)}`}
          icon={<Clock3 />}
          tone="primary"
        />
      </StatGrid>

      {/* Callout */}
      {data && data.errorRequests > 0 && errorRate >= 0.05 && (
        <Callout tone={errorRate >= 0.2 ? 'error' : 'warning'}>
          当前错误率 {formatPercent(errorRate)}，共 {formatNumber(data.errorRequests)} 次错误请求，建议查看明细。
        </Callout>
      )}

      {/* 视图切换 Tabs */}
      <div className="inline-flex overflow-hidden rounded-lg border border-border">
        <Button
          variant={activeTab === 'trend' ? 'default' : 'ghost'}
          size="sm"
          className="rounded-none"
          onClick={() => setActiveTab('trend')}
        >
          <BarChart3 className="h-3.5 w-3.5" />趋势图
        </Button>
        <Button
          variant={activeTab === 'records' ? 'default' : 'ghost'}
          size="sm"
          className="rounded-none"
          onClick={() => setActiveTab('records')}
        >
          <Activity className="h-3.5 w-3.5" />明细记录
        </Button>
      </div>

      {/* 内容区 */}
      {activeTab === 'trend' ? (
        <TrendView />
      ) : (
        <RecordsView onViewDetail={setSelectedRecord} />
      )}

      {/* 底部状态 */}
      <div className="rounded-xl border border-border bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
        <span>用量 · </span>
        <span>{autoRefresh.enabled ? `每 ${autoRefresh.intervalSeconds} 秒自动刷新` : '自动刷新已关闭'}</span>
        {cleanupStatus.data?.status === 'running' && (
          <span className="ml-2 text-warning">· 清理任务执行中...</span>
        )}
      </div>

      {/* 弹窗 */}
      <UsageDetailModal
        record={selectedRecord}
        open={Boolean(selectedRecord)}
        onClose={() => setSelectedRecord(null)}
      />
      <UsageCleanupModal
        open={cleanupOpen}
        onClose={() => setCleanupOpen(false)}
      />
    </PageContainer>
  )
}
