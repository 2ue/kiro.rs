import * as React from 'react'
import { Info, LayoutGrid, List, Trash2, X } from 'lucide-react'
import { useQuery } from '@tanstack/react-query'
import { formatDate, formatNumber, formatPercent, formatUsd, ratio } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import { getExternalPools } from '@/api/credentials'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useCredentials } from '@/hooks/use-credentials'
import { useUsageRecordsPage, useUsageSummary } from '@/hooks/use-usage'
import type {
  UsageRecord,
  UsageRecordsPageQuery,
  UsageRecordStatus,
  UsageSource,
} from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  EmptyState,
  ErrorState,
  LoadingState,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Card,
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
import { cn } from '@/lib/utils'
import {
  UsageMetric,
  formatAttemptChain,
  formatAttemptSummary,
  formatExternalAttemptChain,
  formatLatency,
  routeLabel,
  routeTone,
  sourceLabel,
  statusLabel,
  statusTone,
  upstreamModelLabel,
} from './usage-helpers'
import { UsageBillingModal, UsageCleanupModal, UsageDetailModal } from './usage-modals'

const USAGE_AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:usage'

export function UsagePage() {
  const [searchText, setSearchText] = React.useState('')
  const [model, setModel] = React.useState('')
  const [endpoint, setEndpoint] = React.useState('')
  const [conversationId, setConversationId] = React.useState('')
  const [routeTarget, setRouteTarget] = React.useState('')
  const [status, setStatus] = React.useState<UsageRecordStatus | ''>('')
  const [source, setSource] = React.useState<UsageSource | ''>('')
  const [streamMode, setStreamMode] = React.useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = React.useState('')
  const [selectedRecord, setSelectedRecord] = React.useState<UsageRecord | null>(null)
  const [billingRecord, setBillingRecord] = React.useState<UsageRecord | null>(null)
  const [cleanupOpen, setCleanupOpen] = React.useState(false)
  const [recordView, setRecordView] = React.useState<'cards' | 'table'>('table')
  const [page, setPage] = React.useState(1)
  const limit = 20
  const autoRefresh = useAutoRefreshPreference(USAGE_AUTO_REFRESH_KEY)

  const query = React.useMemo<UsageRecordsPageQuery>(() => {
    const next: UsageRecordsPageQuery = { page, limit }
    if (searchText.trim()) next.q = searchText.trim()
    if (model.trim()) next.model = model.trim()
    if (endpoint.trim()) next.endpoint = endpoint.trim()
    if (conversationId.trim()) next.conversationId = conversationId.trim()
    const [routeType, routeId] = routeTarget.split(':')
    const parsedRouteId = Number(routeId)
    if (routeTarget && Number.isFinite(parsedRouteId)) {
      if (routeType === 'credential') next.credentialId = parsedRouteId
      if (routeType === 'external') next.externalPoolId = parsedRouteId
    }
    if (status) next.status = status
    if (source) next.source = source
    if (streamMode !== 'all') next.stream = streamMode === 'stream'
    if (minCacheRead.trim() && Number.isFinite(Number(minCacheRead))) next.minCacheRead = Number(minCacheRead)
    return next
  }, [conversationId, endpoint, minCacheRead, model, page, routeTarget, searchText, source, status, streamMode])

  const summary = useUsageSummary(autoRefresh.refetchInterval)
  const records = useUsageRecordsPage(query, autoRefresh.refetchInterval)
  const credentials = useCredentials({ refetchInterval: autoRefresh.refetchInterval })
  const externalPools = useQuery({
    queryKey: ['external-pools'],
    queryFn: getExternalPools,
    refetchInterval: autoRefresh.refetchInterval,
  })

  React.useEffect(() => {
    setPage(1)
  }, [conversationId, endpoint, minCacheRead, model, routeTarget, searchText, source, status, streamMode])

  const credentialLabels = React.useMemo(() => {
    const labels = new Map<number, string>()
    for (const credential of credentials.data?.credentials || []) {
      labels.set(credential.id, credential.email || credential.maskedApiKey || `账号 #${credential.id}`)
    }
    return labels
  }, [credentials.data?.credentials])

  const hasFilters = Boolean(
    searchText || model || endpoint || conversationId || routeTarget || status || source || streamMode !== 'all' || minCacheRead
  )
  const pageRecords = records.data?.records || []
  const hasNext = Boolean(records.data?.hasNext)
  const recordsPage = records.data?.page
  const pageTransitionPending =
    recordsPage !== undefined && (records.isPlaceholderData || (records.isFetching && recordsPage !== page))
  const summaryData = summary.data
  const readRatio = ratio(
    summaryData?.localPromptCacheReadInputTokens || 0,
    summaryData?.localPromptCacheInputTokens || 0
  )
  const cachedRatio = ratio(
    (summaryData?.localPromptCacheReadInputTokens || 0) +
      (summaryData?.localPromptCacheCreationInputTokens || 0),
    summaryData?.localPromptCacheInputTokens || 0
  )
  const pricedRatio = ratio(summaryData?.pricedRequests || 0, summaryData?.totalRequests || 0)
  const realtime = summaryData?.realtime
  const realtimeWindow = realtime?.windowSeconds || 60

  const resetFilters = () => {
    setSearchText('')
    setModel('')
    setEndpoint('')
    setConversationId('')
    setRouteTarget('')
    setStatus('')
    setSource('')
    setStreamMode('all')
    setMinCacheRead('')
  }

  return (
    <PageContainer>
      <PageHeader title={pageMeta.usage.title} subtitle={pageMeta.usage.subtitle} />

      <StatGrid>
        <StatCard title="请求总数" value={formatNumber(summaryData?.totalRequests || 0)} />
        <StatCard
          title="实时 RPM"
          value={formatNumber(realtime?.rpm || 0)}
          desc={`近 ${realtimeWindow} 秒 ${formatNumber(realtime?.requests || 0)} 请求`}
          tone="info"
        />
        <StatCard
          title="实时 TPM"
          value={formatNumber(realtime?.totalTpm || 0)}
          desc="按展示输入 + 展示输出统计"
          tone="info"
        />
        <StatCard title="缓存命中较高" value={formatNumber(summaryData?.highCacheRequests || 0)} tone="success" />
        <StatCard
          title="缓存读取"
          value={formatNumber(summaryData?.totalCacheReadInputTokens || 0)}
          desc={`本地读取 ${formatPercent(readRatio)} / 总缓存 ${formatPercent(cachedRatio)}`}
        />
        <StatCard
          title="估算费用"
          value={formatUsd(summaryData?.totalEstimatedCostUsd || 0)}
          desc={`已计价 ${formatPercent(pricedRatio)}`}
          tone="info"
        />
      </StatGrid>

      <SectionCard
        title="使用记录"
        description="错误详情和账号切换链路可点击查看。"
        actions={
          <>
            <div className="inline-flex overflow-hidden rounded-lg border border-border">
              <Button
                variant={recordView === 'cards' ? 'default' : 'ghost'}
                size="sm"
                className="rounded-none"
                onClick={() => setRecordView('cards')}
              >
                <LayoutGrid className="size-4" />
                卡片
              </Button>
              <Button
                variant={recordView === 'table' ? 'default' : 'ghost'}
                size="sm"
                className="rounded-none"
                onClick={() => setRecordView('table')}
              >
                <List className="size-4" />
                表格
              </Button>
            </div>
            <label className="flex items-center gap-2 text-xs text-muted-foreground">
              <Switch checked={autoRefresh.enabled} onCheckedChange={autoRefresh.setEnabled} />
              自动刷新
            </label>
            <Input
              type="number"
              min={5}
              max={3600}
              className="h-8 w-20"
              value={autoRefresh.intervalSeconds}
              disabled={!autoRefresh.enabled}
              onChange={(e) => autoRefresh.setIntervalSeconds(Number(e.target.value))}
            />
            <span className="text-xs text-muted-foreground">秒</span>
            <Button variant="outline" size="sm" onClick={resetFilters} disabled={!hasFilters}>
              <X className="size-4" />
              重置
            </Button>
            <Button variant="outline" size="sm" onClick={() => setCleanupOpen(true)}>
              <Trash2 className="size-4" />
              分批清理
            </Button>
          </>
        }
      >
        <div className="mb-3 grid gap-2 md:grid-cols-2 xl:grid-cols-4">
          <Input
            className="xl:col-span-2"
            value={searchText}
            onChange={(e) => setSearchText(e.target.value)}
            placeholder="搜索模型、账号、会话、路径、错误"
          />
          <Input value={model} onChange={(e) => setModel(e.target.value)} placeholder="模型" />
          <Input
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
            placeholder="入口路径，如 /cc/v1/messages"
          />
          <Input
            value={conversationId}
            onChange={(e) => setConversationId(e.target.value)}
            placeholder="会话 ID"
          />
          <Select value={routeTarget || 'all'} onValueChange={(v) => setRouteTarget(v === 'all' ? '' : v)}>
            <SelectTrigger size="sm">
              <SelectValue placeholder="全部账号/外部账号" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">全部账号/外部账号</SelectItem>
              {(credentials.data?.credentials || []).map((credential) => (
                <SelectItem key={`credential:${credential.id}`} value={`credential:${credential.id}`}>
                  账号 #{credential.id} {credential.email || credential.maskedApiKey || '未命名账号'}
                </SelectItem>
              ))}
              {(externalPools.data?.pools || []).map((pool) => (
                <SelectItem key={`external:${pool.id}`} value={`external:${pool.id}`}>
                  外部账号 #{pool.id} {pool.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Select value={status || 'all'} onValueChange={(v) => setStatus(v === 'all' ? '' : (v as UsageRecordStatus))}>
            <SelectTrigger size="sm">
              <SelectValue placeholder="全部状态" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">全部状态</SelectItem>
              <SelectItem value="success">成功</SelectItem>
              <SelectItem value="error">错误</SelectItem>
              <SelectItem value="stream_error">流错误</SelectItem>
              <SelectItem value="upstream_timeout">服务超时</SelectItem>
              <SelectItem value="client_dropped">客户端断开</SelectItem>
            </SelectContent>
          </Select>
          <Select value={source || 'all'} onValueChange={(v) => setSource(v === 'all' ? '' : (v as UsageSource))}>
            <SelectTrigger size="sm">
              <SelectValue placeholder="全部来源" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">全部来源</SelectItem>
              <SelectItem value="upstream_metadata">服务返回用量</SelectItem>
              <SelectItem value="local_prompt_cache">本地缓存估算</SelectItem>
              <SelectItem value="context_estimate">上下文估算</SelectItem>
              <SelectItem value="request_estimate">请求估算</SelectItem>
              <SelectItem value="none">无缓存</SelectItem>
            </SelectContent>
          </Select>
          <Select value={streamMode} onValueChange={(v) => setStreamMode(v as 'all' | 'stream' | 'non_stream')}>
            <SelectTrigger size="sm">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">全部请求</SelectItem>
              <SelectItem value="stream">Stream</SelectItem>
              <SelectItem value="non_stream">非 Stream</SelectItem>
            </SelectContent>
          </Select>
          <Input
            value={minCacheRead}
            onChange={(e) => setMinCacheRead(e.target.value)}
            placeholder="最小 cache read"
            inputMode="numeric"
          />
        </div>

        {records.isLoading ? (
          <LoadingState />
        ) : records.error ? (
          <ErrorState message={extractErrorMessage(records.error)} />
        ) : pageRecords.length === 0 ? (
          <EmptyState title={page === 1 ? '暂无记录' : '当前页暂无记录'} />
        ) : recordView === 'table' ? (
          <div className="scrollbar-thin overflow-x-auto">
            <Table className="min-w-[1180px]">
              <TableHeader>
                <TableRow>
                  <TableHead>时间 / 状态</TableHead>
                  <TableHead>模型 / 入口</TableHead>
                  <TableHead>账号</TableHead>
                  <TableHead>Token</TableHead>
                  <TableHead>缓存</TableHead>
                  <TableHead>费用</TableHead>
                  <TableHead>耗时</TableHead>
                  <TableHead>调用链路</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {pageRecords.map((record) => (
                  <UsageTableRow
                    key={record.id}
                    record={record}
                    credentialLabels={credentialLabels}
                    onDetail={setSelectedRecord}
                    onBilling={setBillingRecord}
                  />
                ))}
              </TableBody>
            </Table>
          </div>
        ) : (
          <div className="grid gap-3">
            {pageRecords.map((record) => (
              <UsageRecordCard
                key={record.id}
                record={record}
                credentialLabels={credentialLabels}
                onDetail={setSelectedRecord}
                onBilling={setBillingRecord}
              />
            ))}
          </div>
        )}

        {(page > 1 || hasNext || pageTransitionPending) && (
          <div className="mt-4 flex items-center justify-center gap-3">
            <Button
              variant="outline"
              size="sm"
              disabled={page === 1 || pageTransitionPending}
              onClick={() => setPage((v) => Math.max(1, v - 1))}
            >
              上一页
            </Button>
            <span className="text-sm text-muted-foreground">
              第 {page} 页，每页 {limit} 条
            </span>
            <Button
              variant="outline"
              size="sm"
              disabled={!hasNext || pageTransitionPending}
              onClick={() => setPage((v) => v + 1)}
            >
              下一页
            </Button>
          </div>
        )}
      </SectionCard>

      <UsageBillingModal record={billingRecord} onClose={() => setBillingRecord(null)} />
      <UsageDetailModal record={selectedRecord} onClose={() => setSelectedRecord(null)} />
      <UsageCleanupModal open={cleanupOpen} onClose={() => setCleanupOpen(false)} />
    </PageContainer>
  )
}

interface RowProps {
  record: UsageRecord
  credentialLabels: Map<number, string>
  onDetail: (record: UsageRecord) => void
  onBilling: (record: UsageRecord) => void
}

function rowMetrics(record: UsageRecord) {
  const reportedInputTotal =
    record.compatInputTokens + record.cacheReadInputTokens + record.cacheCreationInputTokens
  return {
    rowReadRatio: ratio(record.cacheReadInputTokens, reportedInputTotal),
    rowCachedRatio: ratio(
      record.cacheReadInputTokens + record.cacheCreationInputTokens,
      reportedInputTotal
    ),
  }
}

function UsageTableRow({ record, credentialLabels, onDetail, onBilling }: RowProps) {
  const label =
    typeof record.credentialId === 'number'
      ? credentialLabels.get(record.credentialId) || record.credentialLabel
      : record.credentialLabel
  const { rowReadRatio, rowCachedRatio } = rowMetrics(record)
  const attemptChain = formatAttemptChain(record)
  const attemptSummary = formatAttemptSummary(record)
  const externalAttemptChain = formatExternalAttemptChain(record)
  const isExternal = record.routeKind === 'external_pool'

  return (
    <TableRow>
      <TableCell>
        <div className="font-medium text-foreground/80">{formatDate(record.createdAt)}</div>
        <div className="mt-1 flex flex-wrap items-center gap-1">
          <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
          <Badge tone={record.stream ? 'secondary' : 'neutral'}>
            {record.stream ? 'stream' : 'non-stream'}
          </Badge>
          <Badge tone={routeTone(record)}>{routeLabel(record)}</Badge>
        </div>
      </TableCell>
      <TableCell className="min-w-0">
        <div className="max-w-[260px] truncate font-semibold" title={record.model || '-'}>
          请求 {record.model || '-'}
        </div>
        <div className="max-w-[260px] truncate text-xs text-muted-foreground" title={upstreamModelLabel(record)}>
          实际模型 {upstreamModelLabel(record)}
        </div>
        <div className="mt-1 flex max-w-[260px] flex-wrap items-center gap-1">
          <Badge>{record.endpoint || '-'}</Badge>
          {record.stickyBound && <Badge tone="secondary">sticky</Badge>}
          {record.fallbackFromSticky && <Badge tone="warning">sticky回退</Badge>}
        </div>
      </TableCell>
      <TableCell>
        <div className="font-semibold">
          {isExternal ? `外部账号 #${record.externalPoolId ?? '-'}` : `#${record.credentialId ?? '-'}`}
        </div>
        {label && (
          <div className="max-w-[180px] truncate text-xs text-muted-foreground" title={label}>
            {label}
          </div>
        )}
        {isExternal && record.externalPoolName && (
          <div className="max-w-[180px] truncate text-xs text-muted-foreground" title={record.externalPoolName}>
            {record.externalPoolName}
          </div>
        )}
      </TableCell>
      <TableCell className="font-mono text-xs">
        <div>展示输入 {formatNumber(record.compatInputTokens)}</div>
        <div className="text-muted-foreground">展示输出 {formatNumber(record.outputTokens)}</div>
      </TableCell>
      <TableCell className="font-mono text-xs">
        <div className="text-success">读 {formatNumber(record.cacheReadInputTokens)}</div>
        <div className="text-info">写 {formatNumber(record.cacheCreationInputTokens)}</div>
        <div className="text-muted-foreground">
          {formatPercent(rowReadRatio)} / {formatPercent(rowCachedRatio)}
        </div>
        <div className="mt-1">
          <Badge tone={record.simulated ? 'warning' : 'secondary'}>{sourceLabel(record.usageSource)}</Badge>
        </div>
      </TableCell>
      <TableCell>
        <button
          type="button"
          className="font-semibold text-primary underline-offset-2 hover:underline"
          onClick={() => onBilling(record)}
          title="查看计费明细"
        >
          {formatUsd(record.estimatedCostUsd || 0)}
        </button>
      </TableCell>
      <TableCell>
        <div className="font-semibold">{formatLatency(record.durationMs)}</div>
        <div className="text-xs text-muted-foreground">首字 {formatLatency(record.firstTokenLatencyMs)}</div>
      </TableCell>
      <TableCell>
        {attemptChain ? (
          <button
            type="button"
            className="max-w-[220px] truncate text-left text-xs font-medium text-primary hover:underline"
            title={`${attemptSummary} · ${attemptChain}`}
            onClick={() => onDetail(record)}
          >
            {attemptSummary}
          </button>
        ) : (
          <span className="text-xs text-muted-foreground/60">-</span>
        )}
        {externalAttemptChain && (
          <button
            type="button"
            className="mt-1 block max-w-[220px] truncate text-left text-xs font-medium text-primary hover:underline"
            title={externalAttemptChain}
            onClick={() => onDetail(record)}
          >
            {externalAttemptChain}
          </button>
        )}
        {record.errorMessage && (
          <button
            type="button"
            className="mt-1 block max-w-[220px] truncate text-left text-xs text-destructive hover:underline"
            title={record.errorDetail || record.errorMessage}
            onClick={() => onDetail(record)}
          >
            {record.errorMessage}
          </button>
        )}
      </TableCell>
      <TableCell className="text-right">
        <button
          type="button"
          className="inline-flex size-7 items-center justify-center text-muted-foreground transition hover:text-primary"
          onClick={() => onDetail(record)}
          title="查看用量口径和详情"
        >
          <Info className="size-4" />
        </button>
      </TableCell>
    </TableRow>
  )
}

function UsageRecordCard({ record, credentialLabels, onDetail, onBilling }: RowProps) {
  const label =
    typeof record.credentialId === 'number'
      ? credentialLabels.get(record.credentialId) || record.credentialLabel
      : record.credentialLabel
  const { rowReadRatio, rowCachedRatio } = rowMetrics(record)
  const attemptChain = formatAttemptChain(record)
  const attemptSummary = formatAttemptSummary(record)
  const externalAttemptChain = formatExternalAttemptChain(record)
  const isExternal = record.routeKind === 'external_pool'

  return (
    <Card className="p-3">
      <div className="flex flex-col gap-2.5">
        <div className="flex flex-col gap-2 xl:flex-row xl:items-start xl:justify-between">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-1.5">
              <span className="text-xs font-medium text-muted-foreground">{formatDate(record.createdAt)}</span>
              <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
              <Badge tone={record.simulated ? 'warning' : 'secondary'}>{sourceLabel(record.usageSource)}</Badge>
              <Badge tone={record.stream ? 'secondary' : 'neutral'}>
                {record.stream ? 'stream' : 'non-stream'}
              </Badge>
              <Badge tone={routeTone(record)}>{routeLabel(record)}</Badge>
            </div>
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              <span className="max-w-[360px] truncate text-sm font-semibold" title={record.model || '-'}>
                请求 {record.model || '-'}
              </span>
              <span className="max-w-[360px] truncate text-xs text-muted-foreground" title={upstreamModelLabel(record)}>
                实际模型 {upstreamModelLabel(record)}
              </span>
              <Badge>{record.endpoint || '-'}</Badge>
              {record.stickyBound && <Badge tone="secondary">sticky</Badge>}
              {record.fallbackFromSticky && <Badge tone="warning">sticky回退</Badge>}
            </div>
          </div>
          <div className="flex shrink-0 flex-wrap items-center gap-2 text-sm">
            <button
              type="button"
              className="font-semibold text-primary underline-offset-2 hover:underline"
              onClick={() => onBilling(record)}
              title="查看计费明细"
            >
              {formatUsd(record.estimatedCostUsd || 0)}
            </button>
            <div className="text-right">
              <div className="font-semibold">{formatLatency(record.durationMs)}</div>
              <div className="text-xs text-muted-foreground">首字 {formatLatency(record.firstTokenLatencyMs)}</div>
            </div>
            <button
              type="button"
              className="inline-flex size-7 items-center justify-center text-muted-foreground transition hover:text-primary"
              onClick={() => onDetail(record)}
              title="查看用量口径和详情"
            >
              <Info className="size-4" />
            </button>
          </div>
        </div>

        <div className="grid gap-2 text-sm md:grid-cols-2 xl:grid-cols-[220px_1fr]">
          <div className="min-w-0 rounded-lg bg-muted/60 px-2.5 py-1.5">
            <div className="text-xs text-muted-foreground">{isExternal ? '外部账号' : '账号'}</div>
            <div className="font-semibold">
              {isExternal ? `#${record.externalPoolId ?? '-'}` : `#${record.credentialId ?? '-'}`}
            </div>
            {label && (
              <div className="truncate text-xs text-muted-foreground" title={label}>
                {label}
              </div>
            )}
            {isExternal && record.externalPoolName && (
              <div className="truncate text-xs text-muted-foreground" title={record.externalPoolName}>
                {record.externalPoolName}
              </div>
            )}
          </div>
          <div className="min-w-0 rounded-lg bg-muted/60 px-2.5 py-1.5">
            <div className="text-xs text-muted-foreground">会话</div>
            <div className="truncate font-mono text-xs" title={record.conversationId || '-'}>
              {record.conversationId || '-'}
            </div>
            {attemptChain && (
              <button
                type="button"
                className="mt-1 max-w-full truncate text-left text-xs font-medium text-primary hover:underline"
                title={`${attemptSummary} · ${attemptChain}`}
                onClick={() => onDetail(record)}
              >
                调用链路 {attemptSummary}
              </button>
            )}
            {externalAttemptChain && (
              <button
                type="button"
                className="mt-1 block max-w-full truncate text-left text-xs font-medium text-primary hover:underline"
                title={externalAttemptChain}
                onClick={() => onDetail(record)}
              >
                外部链路 {externalAttemptChain}
              </button>
            )}
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
          <UsageMetric label="展示输入" value={formatNumber(record.compatInputTokens)} />
          <UsageMetric label="展示缓存写入" value={formatNumber(record.cacheCreationInputTokens)} tone="info" />
          <UsageMetric label="展示缓存读取" value={formatNumber(record.cacheReadInputTokens)} tone="success" />
          <UsageMetric label="展示输出" value={formatNumber(record.outputTokens)} />
          <UsageMetric label="读取率" value={formatPercent(rowReadRatio)} />
          <UsageMetric label="缓存率" value={formatPercent(rowCachedRatio)} />
        </div>

        {record.errorMessage && (
          <button
            type="button"
            className={cn(
              'rounded-lg border border-destructive/20 bg-destructive/5 px-2.5 py-1.5 text-left text-xs text-destructive hover:bg-destructive/10'
            )}
            onClick={() => onDetail(record)}
            title={record.errorDetail || record.errorMessage}
          >
            <span className="font-semibold">错误详情：</span>
            <span className="line-clamp-2">{record.errorMessage}</span>
          </button>
        )}
      </div>
    </Card>
  )
}
