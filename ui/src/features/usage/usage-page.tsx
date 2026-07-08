import { useMemo, useState } from 'react'
import {
  Activity,
  BarChart3,
  ChevronDown,
  ChevronUp,
  Clock3,
  Database,
  Info,
  RefreshCw,
  Trash2,
  X,
  Zap,
} from 'lucide-react'
import { useQuery } from '@tanstack/react-query'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { useCredentials } from '@/hooks/use-credentials'
import {
  useUsageDashboardSeries,
  useUsageSummary,
  useUsageRecordsPage,
  useUsageCleanupStatus,
  useRefreshUsageQueriesAfterCleanup,
} from '@/hooks/use-usage'
import { getExternalPools } from '@/api/credentials'
import { formatDate, formatCompact, formatMeteringUsage, formatNumber, formatPercent, formatUsd, ratio } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type { UsageRecord, UsageRecordStatus, UsageRecordsPageQuery, UsageSource, UsageSeriesPoint } from '@/types/api'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
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
import {
  statusLabel,
  statusTone,
  routeLabel,
  routeTone,
  formatLatency,
  sourceLabel,
  formatAttemptSummary,
  formatAttemptChain,
  formatExternalAttemptChain,
  upstreamModelLabel,
} from './usage-helpers'
import { UsageDetailModal } from './usage-detail-modal'
import { UsageCleanupModal } from './usage-cleanup-modal'

// ─── 常量 ─────────────────────────────────────────────────────────────────────

const AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:usage'
const PAGE_SIZE = 20
const REQUEST_ID_PATTERN = /req_[A-Za-z0-9_-]+/

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

function usageInputTotal(record: UsageRecord): number {
  return record.compatInputTokens + record.cacheReadInputTokens + record.cacheCreationInputTokens
}

function routeTargetLabel(record: UsageRecord, credentialLabel?: string): string {
  if (record.routeKind === 'external_pool') {
    const name = record.externalPoolName ? ` ${record.externalPoolName}` : ''
    return `外部账号 #${record.externalPoolId ?? '-'}${name}`
  }
  const label = credentialLabel ? ` ${credentialLabel}` : ''
  return `账号 #${record.credentialId ?? '-'}${label}`
}

function extractRequestId(value: string): string {
  return value.match(REQUEST_ID_PATTERN)?.[0] ?? ''
}

// ─── 趋势图区 ─────────────────────────────────────────────────────────────────

function TrendView() {
  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)
  const dashboardSeries = useUsageDashboardSeries('Asia/Shanghai', autoRefresh.refetchInterval)
  const series = dashboardSeries.data?.series

  const hourlyData = useMemo(() => (series?.hourly24h ?? []).map(seriesPointToRow), [series?.hourly24h])
  const dailyData = useMemo(() => (series?.daily7d ?? []).map(seriesPointToRow), [series?.daily7d])

  if (dashboardSeries.isLoading) return <LoadingState text="加载趋势数据..." className="py-12" />
  if (dashboardSeries.error) return <ErrorState title="趋势加载失败" message={extractErrorMessage(dashboardSeries.error)} />

  return (
    <div className="space-y-3">
      <div className="grid gap-3 xl:grid-cols-2">
        <SectionCard
          title="最近 24 小时（按小时）"
          description="请求量与错误趋势"
          actions={
            hourlyData.length > 0
              ? <Badge tone="neutral" title={formatNumber(hourlyData.reduce((s, r) => s + Number(r.requests), 0))}>{formatCompact(hourlyData.reduce((s, r) => s + Number(r.requests), 0))} 请求</Badge>
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
  autoRefreshInterval,
}: {
  onViewDetail: (r: UsageRecord) => void
  autoRefreshInterval: number | false
}) {
  const [page, setPage] = useState(1)
  const [q, setQ] = useState('')
  const [requestId, setRequestId] = useState('')
  const [model, setModel] = useState('')
  const [endpoint, setEndpoint] = useState('')
  const [conversationId, setConversationId] = useState('')
  const [routeTarget, setRouteTarget] = useState('')
  const [status, setStatus] = useState<UsageRecordStatus | '__all__'>('__all__')
  const [source, setSource] = useState<UsageSource | '__all__'>('__all__')
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = useState('')

  // 文本筛选项防抖,避免每次按键都触发查询(展示值即时,查询用防抖值)
  const qD = useDebouncedValue(q)
  const requestIdD = useDebouncedValue(requestId)
  const modelD = useDebouncedValue(model)
  const endpointD = useDebouncedValue(endpoint)
  const conversationIdD = useDebouncedValue(conversationId)
  const minCacheReadD = useDebouncedValue(minCacheRead)

  const credentials = useCredentials({ refetchInterval: autoRefreshInterval })
  const externalPools = useQuery({
    queryKey: ['external-pools'],
    queryFn: getExternalPools,
    refetchInterval: autoRefreshInterval,
  })

  const credentialLabels = useMemo(() => {
    const labels = new Map<number, string>()
    for (const c of credentials.data?.credentials ?? []) {
      labels.set(c.id, c.email || c.maskedApiKey || `账号 #${c.id}`)
    }
    return labels
  }, [credentials.data?.credentials])

  const query = useMemo<UsageRecordsPageQuery>(() => {
    const next: UsageRecordsPageQuery = { page, limit: PAGE_SIZE }
    const qValue = qD.trim()
    const requestIdInput = requestIdD.trim()
    const requestIdValue = extractRequestId(requestIdInput) || requestIdInput || extractRequestId(qValue)
    if (requestIdValue) {
      next.requestId = requestIdValue
      return next
    }
    if (qValue) next.q = qValue
    if (modelD.trim()) next.model = modelD.trim()
    if (endpointD.trim()) next.endpoint = endpointD.trim()
    if (conversationIdD.trim()) next.conversationId = conversationIdD.trim()
    const [routeType, routeId] = routeTarget.split(':')
    const parsedRouteId = Number(routeId)
    if (routeTarget && Number.isFinite(parsedRouteId)) {
      if (routeType === 'credential') next.credentialId = parsedRouteId
      if (routeType === 'external') next.externalPoolId = parsedRouteId
    }
    if (status !== '__all__') next.status = status
    if (source !== '__all__') next.source = source
    if (streamMode !== 'all') next.stream = streamMode === 'stream'
    if (minCacheReadD.trim() && Number.isFinite(Number(minCacheReadD))) next.minCacheRead = Number(minCacheReadD)
    return next
  }, [conversationIdD, endpointD, minCacheReadD, modelD, page, qD, requestIdD, routeTarget, source, status, streamMode])

  const records = useUsageRecordsPage(query, autoRefreshInterval)
  const items = records.data?.records ?? []
  const hasNext = records.data?.hasNext ?? false
  const hasFilters =
    status !== '__all__' || source !== '__all__' || streamMode !== 'all' ||
    !!q.trim() || !!requestId.trim() || !!model.trim() || !!endpoint.trim() || !!conversationId.trim() ||
    !!routeTarget || !!minCacheRead.trim()

  const clearFilters = () => {
    setQ(''); setRequestId(''); setModel(''); setEndpoint(''); setConversationId('')
    setRouteTarget(''); setStatus('__all__'); setSource('__all__')
    setStreamMode('all'); setMinCacheRead('')
  }

  return (
    <div className="space-y-3">
      <SectionCard
        title="明细记录"
        description="每次请求的完整记录，点击行查看详情"
        noPadding
      >
        <div className="px-4 pt-4 pb-2">
          <Toolbar>
            <ToolbarSearch value={q} onChange={(v) => { setQ(v); setPage(1) }} placeholder="搜索模型、账号、会话、路径、错误、req_..." />
            <ToolbarActions>
              {hasFilters && (
                <Button variant="ghost" size="sm" onClick={() => { clearFilters(); setPage(1) }}>
                  <X className="h-3.5 w-3.5" />重置
                </Button>
              )}
              {records.isFetching && <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />}
            </ToolbarActions>
          </Toolbar>

          <div className="mt-2 rounded-lg bg-muted/30 p-3">
            <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
              <Input
                placeholder="请求 ID / req_..."
                value={requestId}
                onChange={(e) => { setRequestId(e.target.value); setPage(1) }}
                className="h-8 text-xs font-mono"
              />
              <Input
                placeholder="模型"
                value={model}
                onChange={(e) => { setModel(e.target.value); setPage(1) }}
                className="h-8 text-xs"
              />
              <Input
                placeholder="入口路径，如 /cc/v1/messages"
                value={endpoint}
                onChange={(e) => { setEndpoint(e.target.value); setPage(1) }}
                className="h-8 text-xs"
              />
              <Input
                placeholder="会话 ID"
                value={conversationId}
                onChange={(e) => { setConversationId(e.target.value); setPage(1) }}
                className="h-8 text-xs"
              />
              <Input
                placeholder="最小缓存读取 token"
                value={minCacheRead}
                onChange={(e) => { setMinCacheRead(e.target.value); setPage(1) }}
                className="h-8 text-xs"
                inputMode="numeric"
              />
              <Select value={status} onValueChange={(v) => { setStatus(v as UsageRecordStatus | '__all__'); setPage(1) }}>
                <SelectTrigger size="sm"><SelectValue placeholder="全部状态" /></SelectTrigger>
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
                <SelectTrigger size="sm"><SelectValue placeholder="全部来源" /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部来源</SelectItem>
                  <SelectItem value="upstream_metadata">服务返回用量</SelectItem>
                  <SelectItem value="local_prompt_cache">本地缓存估算</SelectItem>
                  <SelectItem value="context_estimate">上下文估算</SelectItem>
                  <SelectItem value="request_estimate">请求估算</SelectItem>
                  <SelectItem value="none">无缓存</SelectItem>
                </SelectContent>
              </Select>
              <Select value={streamMode} onValueChange={(v) => { setStreamMode(v as 'all' | 'stream' | 'non_stream'); setPage(1) }}>
                <SelectTrigger size="sm"><SelectValue placeholder="全部请求" /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">全部请求</SelectItem>
                  <SelectItem value="stream">Stream</SelectItem>
                  <SelectItem value="non_stream">非 Stream</SelectItem>
                </SelectContent>
              </Select>
              <Select value={routeTarget || '__all__'} onValueChange={(v) => { setRouteTarget(v === '__all__' ? '' : v); setPage(1) }}>
                <SelectTrigger size="sm"><SelectValue placeholder="全部账号/外部账号" /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部账号/外部账号</SelectItem>
                  {(credentials.data?.credentials ?? []).map((c) => (
                    <SelectItem key={`credential:${c.id}`} value={`credential:${c.id}`}>
                      账号 #{c.id} {c.email || c.maskedApiKey || ''}
                    </SelectItem>
                  ))}
                  {(externalPools.data?.pools ?? []).map((p) => (
                    <SelectItem key={`external:${p.id}`} value={`external:${p.id}`}>
                      外部账号 #{p.id} {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>
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
              <Table className="min-w-[1280px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>时间 / 状态</TableHead>
                    <TableHead>会话 / 请求</TableHead>
                    <TableHead>模型 / 入口</TableHead>
                    <TableHead>账号 / 路由</TableHead>
                    <TableHead className="text-right">用量</TableHead>
                    <TableHead>缓存</TableHead>
                    <TableHead className="text-right">费用</TableHead>
                    <TableHead className="text-right">耗时</TableHead>
                    <TableHead>链路 / 错误</TableHead>
                    <TableHead className="text-right">操作</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {items.map((record) => {
                    const label = typeof record.credentialId === 'number'
                      ? credentialLabels.get(record.credentialId) || record.credentialLabel
                      : record.credentialLabel
                    const reportedInputTotal = usageInputTotal(record)
                    const rowReadRatio = ratio(record.cacheReadInputTokens, reportedInputTotal)
                    const rowCachedRatio = ratio(
                      record.cacheReadInputTokens + record.cacheCreationInputTokens,
                      reportedInputTotal,
                    )
                    const attemptSummary = formatAttemptSummary(record)
                    const attemptChain = formatAttemptChain(record)
                    const externalChain = formatExternalAttemptChain(record)
                    const targetLabel = routeTargetLabel(record, label)
                    const resolvedModel = upstreamModelLabel(record)
                    const hasModelChange = resolvedModel !== '-' && resolvedModel !== record.model
                    return (
                      <TableRow key={record.id} className="cursor-pointer" onClick={() => onViewDetail(record)}>
                        {/* 时间 / 状态 */}
                        <TableCell>
                          <div className="tabular-nums text-xs text-muted-foreground">{formatDate(record.createdAt)}</div>
                          <div className="mt-1 flex flex-wrap gap-1">
                            <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
                            <Badge tone={record.stream ? 'info' : 'neutral'}>{record.stream ? 'stream' : 'non-stream'}</Badge>
                            <Badge tone={routeTone(record)}>{routeLabel(record)}</Badge>
                          </div>
                        </TableCell>
                        {/* 会话 / 请求 */}
                        <TableCell>
                          <div
                            className="max-w-[180px] truncate font-mono text-xs"
                            title={record.conversationId || '-'}
                          >
                            会话 {record.conversationId || '-'}
                          </div>
                          <div
                            className="mt-1 max-w-[180px] truncate font-mono text-[0.62rem] text-muted-foreground/60"
                            title={record.id}
                          >
                            请求 {record.id}
                          </div>
                        </TableCell>
                        {/* 模型 / 入口 */}
                        <TableCell>
                          <div className="max-w-[220px] truncate text-xs font-medium" title={record.model || '-'}>
                            请求 {record.model || '-'}
                          </div>
                          <div
                            className="max-w-[220px] truncate font-mono text-[0.62rem] text-muted-foreground/60"
                            title={resolvedModel}
                          >
                            实际 {hasModelChange ? resolvedModel : '同请求模型'}
                          </div>
                          <div className="mt-1 flex flex-wrap gap-1">
                            <Badge title={record.endpoint || '-'}>{record.endpoint || '-'}</Badge>
                            {record.stickyBound && <Badge tone="secondary">sticky</Badge>}
                            {record.fallbackFromSticky && <Badge tone="warning">sticky回退</Badge>}
                            {record.simulated && <Badge tone="warning">{sourceLabel(record.usageSource)}</Badge>}
                            {!record.simulated && <Badge tone="neutral">{sourceLabel(record.usageSource)}</Badge>}
                          </div>
                        </TableCell>
                        {/* 账号 / 路由 */}
                        <TableCell>
                          <div className="max-w-[190px] truncate text-xs font-semibold" title={targetLabel}>
                            {targetLabel}
                          </div>
                          {record.routeSubtype && (
                            <div className="max-w-[190px] truncate text-[0.68rem] text-muted-foreground/70" title={record.routeSubtype}>
                              {record.routeSubtype}
                            </div>
                          )}
                          {record.fallbackReason && (
                            <div className="max-w-[190px] truncate text-[0.68rem] text-muted-foreground/70" title={record.fallbackReason}>
                              {record.fallbackReason}
                            </div>
                          )}
                        </TableCell>
                        {/* 用量 */}
                        <TableCell className="text-right font-mono text-xs tabular-nums">
                          <div title={formatNumber(record.totalInputTokens)}>本地估算 {formatCompact(record.totalInputTokens)}</div>
                          <div title={formatNumber(record.compatInputTokens)}>展示输入 {formatCompact(record.compatInputTokens)}</div>
                          <div className="text-muted-foreground/60" title={formatNumber(record.outputTokens)}>展示输出 {formatCompact(record.outputTokens)}</div>
                        </TableCell>
                        {/* 缓存 */}
                        <TableCell className="font-mono text-xs tabular-nums">
                          <div className="text-success" title={formatNumber(record.cacheReadInputTokens)}>读 {formatCompact(record.cacheReadInputTokens)}</div>
                          <div className="text-primary" title={formatNumber(record.cacheCreationInputTokens)}>写 {formatCompact(record.cacheCreationInputTokens)}</div>
                          <div className="text-muted-foreground/60">{formatPercent(rowReadRatio)} / {formatPercent(rowCachedRatio)}</div>
                        </TableCell>
                        {/* 费用 */}
                        <TableCell className="text-right font-mono text-xs tabular-nums" onClick={(e) => e.stopPropagation()}>
                          <Button
                            variant="link"
                            size="xs"
                            className="h-auto p-0 font-mono text-xs font-semibold tabular-nums"
                            onClick={() => onViewDetail(record)}
                            title="查看计费明细"
                          >
                            {formatUsd(record.estimatedCostUsd)}
                          </Button>
                          <div className="text-[0.62rem] text-muted-foreground/60">
                            {record.pricingAvailable ? record.pricingModel || '已计价' : '未计价'}
                          </div>
                          <div className="text-[0.62rem] text-muted-foreground/60">
                            Kiro {formatMeteringUsage(record.kiroMeteringUsage)}
                          </div>
                        </TableCell>
                        {/* 耗时 */}
                        <TableCell className="text-right font-mono text-xs tabular-nums">
                          <div>{formatLatency(record.durationMs)}</div>
                          <div className="text-muted-foreground/60">首字 {formatLatency(record.firstTokenLatencyMs)}</div>
                        </TableCell>
                        {/* 调用链路 */}
                        <TableCell>
                          {attemptChain ? (
                            <div
                              className="max-w-[220px] truncate text-xs font-medium text-primary"
                              title={`${attemptSummary} · ${attemptChain}`}
                            >
                              {attemptSummary}
                            </div>
                          ) : null}
                          {externalChain && (
                            <div className="max-w-[220px] truncate text-xs text-muted-foreground" title={externalChain}>
                              {externalChain}
                            </div>
                          )}
                          {(record.publicErrorMessage || record.errorMessage) && (
                            <div
                              className="max-w-[220px] truncate text-xs text-destructive"
                              title={record.publicErrorMessage || record.errorDetail || record.errorMessage}
                            >
                              {record.publicErrorMessage || record.errorMessage}
                            </div>
                          )}
                        </TableCell>
                        {/* 操作 */}
                        <TableCell
                          className="text-right"
                          onClick={(e) => e.stopPropagation()}
                        >
                          <Button
                            variant="ghost"
                            size="icon-xs"
                            onClick={() => onViewDetail(record)}
                            title="查看用量口径和详情"
                          >
                            <Info className="h-3.5 w-3.5" />
                          </Button>
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
            {(page > 1 || hasNext) && (
              <div className="px-4 py-3">
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
  const [trendOpen, setTrendOpen] = useState(true)
  const [selectedRecord, setSelectedRecord] = useState<UsageRecord | null>(null)
  const [cleanupOpen, setCleanupOpen] = useState(false)

  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)
  const summary = useUsageSummary(autoRefresh.refetchInterval)
  const cleanupStatus = useUsageCleanupStatus()
  useRefreshUsageQueriesAfterCleanup(cleanupStatus.data)

  const data = summary.data
  const totalTokens = (data?.totalInputTokens ?? 0) + (data?.totalOutputTokens ?? 0)
  const errorRate = data && data.totalRequests > 0 ? data.errorRequests / data.totalRequests : 0
  const realtime = data?.realtime
  const realtimeWindow = realtime?.windowSeconds ?? 60
  const readRatio = ratio(data?.localPromptCacheReadInputTokens ?? 0, data?.localPromptCacheInputTokens ?? 0)
  const cachedRatio = ratio(
    (data?.localPromptCacheReadInputTokens ?? 0) + (data?.localPromptCacheCreationInputTokens ?? 0),
    data?.localPromptCacheInputTokens ?? 0,
  )

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
          onBlur={(e) => {
            const v = Math.max(5, Math.min(3600, Number(e.target.value) || 30))
            autoRefresh.setIntervalSeconds(v)
          }}
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
      <div className="grid gap-3 grid-cols-2 lg:grid-cols-3">
        <StatCard
          title="总请求"
          value={formatCompact(data?.totalRequests ?? 0)}
          valueTitle={formatNumber(data?.totalRequests ?? 0)}
          desc={
            <span>
              成功 {formatCompact(data?.successRequests ?? 0)} · 错误率{' '}
              <span className={errorRate > 0 ? 'font-semibold text-destructive' : 'font-semibold text-success'}>
                {formatPercent(errorRate)}
              </span>
              {' '}({formatCompact(data?.errorRequests ?? 0)})
            </span>
          }
          icon={<Activity />}
          tone="primary"
        />
        <StatCard
          title="实时 RPM"
          value={formatNumber(realtime?.rpm ?? 0)}
          desc={
            <span>
              TPM {formatCompact(realtime?.totalTpm ?? 0)} · 近 {realtimeWindow} 秒 {formatNumber(realtime?.requests ?? 0)} 请求
            </span>
          }
          icon={<Zap />}
          tone="info"
        />
        <StatCard
          title="缓存命中较高"
          value={formatCompact(data?.highCacheRequests ?? 0)}
          valueTitle={formatNumber(data?.highCacheRequests ?? 0)}
          desc="highCacheThreshold 以上的请求"
          icon={<Zap />}
          tone="success"
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
          title="缓存读取"
          value={formatCompact(data?.totalCacheReadInputTokens ?? 0)}
          valueTitle={formatNumber(data?.totalCacheReadInputTokens ?? 0)}
          desc={`本地读取 ${formatPercent(readRatio)} / 总缓存 ${formatPercent(cachedRatio)}`}
          icon={<Zap />}
          tone="success"
        />
        <StatCard
          title="估算费用"
          value={formatUsd(data?.totalEstimatedCostUsd ?? 0)}
          desc={`计价覆盖 ${formatPercent(ratio(data?.pricedRequests ?? 0, data?.totalRequests ?? 1))}`}
          icon={<Clock3 />}
          tone="primary"
        />
      </div>

      {/* Callout */}
      {data && data.errorRequests > 0 && errorRate >= 0.05 && (
        <Callout tone={errorRate >= 0.2 ? 'error' : 'warning'}>
          当前错误率 {formatPercent(errorRate)}，共 {formatNumber(data.errorRequests)} 次错误请求，建议查看明细。
        </Callout>
      )}

      {/* 趋势图区（可折叠） */}
      <div className="rounded-xl bg-card shadow-sm">
        <Button
          variant="ghost"
          className="flex h-auto w-full items-center justify-between rounded-xl px-4 py-3 text-sm font-medium text-foreground/80 hover:bg-muted/40"
          onClick={() => setTrendOpen((v) => !v)}
        >
          <div className="flex items-center gap-2">
            <BarChart3 className="size-4 text-muted-foreground" />
            趋势图
          </div>
          {trendOpen
            ? <ChevronUp className="size-4 text-muted-foreground" />
            : <ChevronDown className="size-4 text-muted-foreground" />
          }
        </Button>
        {trendOpen && (
          <div className="px-4 pb-4 pt-2">
            <TrendView />
          </div>
        )}
      </div>

      {/* 明细记录（常驻） */}
      <RecordsView onViewDetail={setSelectedRecord} autoRefreshInterval={autoRefresh.refetchInterval} />

      {/* 底部状态 */}
      <div className="rounded-xl bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
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
