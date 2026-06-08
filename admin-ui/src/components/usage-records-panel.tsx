import { useEffect, useMemo, useState } from 'react'
import { DollarSign, Eye, RefreshCw, Trash2, X } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { useCredentials } from '@/hooks/use-credentials'
import {
  useClearUsageRecords,
  useCancelUsageCleanup,
  useModelPricing,
  usePreviewUsageCleanup,
  useStartUsageCleanup,
  useSyncModelPricing,
  useUsageCleanupStatus,
  useUsageRecordsPage,
  useUsageSummary,
} from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
import type { ExternalPoolUsageSnapshot, UsageCleanupMode, UsageCleanupRequest, UsageRecord, UsageRecordsPageQuery, UsageRecordStatus, UsageSource } from '@/types/api'

function formatNumber(value: number): string {
  return new Intl.NumberFormat('zh-CN').format(value)
}

function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return `${(value * 100).toFixed(1)}%`
}

function formatUsd(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: value >= 1 ? 2 : 6,
    maximumFractionDigits: value >= 1 ? 2 : 6,
  }).format(value)
}

function ratio(part: number, total: number): number {
  if (!Number.isFinite(part) || !Number.isFinite(total) || total <= 0) {
    return Number.NaN
  }
  return part / total
}

function formatDate(value?: string): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function sourceLabel(source: UsageSource): string {
  switch (source) {
    case 'upstream_metadata':
      return '上游 metadata'
    case 'local_prompt_cache':
      return '本地 prompt cache'
    case 'context_estimate':
      return '上下文估算'
    case 'request_estimate':
      return '请求估算'
    default:
      return '无缓存'
  }
}

function statusVariant(status: string): 'success' | 'destructive' | 'warning' {
  if (status === 'success') return 'success'
  if (status === 'client_dropped') return 'warning'
  return 'destructive'
}

function statusLabel(status: string): string {
  switch (status) {
    case 'success':
      return '成功'
    case 'error':
      return '错误'
    case 'stream_error':
      return '流错误'
    case 'upstream_timeout':
      return '上游超时'
    case 'client_dropped':
      return '客户端断开'
    default:
      return status
  }
}

function routeLabel(record: UsageRecord): string {
  switch (record.routeSubtype) {
    case 'external_direct_policy':
      return '外部直连'
    case 'external_fallback_preflight':
      return '预检 fallback'
    case 'external_fallback_after_local_attempts':
      return '失败后 fallback'
    case 'external_error':
      return '外部错误'
    case 'local_error_no_fallback':
      return '本地错误'
    case 'local_success':
      return '本地成功'
    default:
      return record.routeKind === 'external_pool' ? '外部池' : '本地'
  }
}

function formatUsageSnapshot(snapshot?: ExternalPoolUsageSnapshot): string {
  if (!snapshot) return '-'
  return [
    `in ${formatNumber(snapshot.inputTokens)}`,
    `out ${formatNumber(snapshot.outputTokens)}`,
    `read ${formatNumber(snapshot.cacheReadInputTokens)}`,
    `create ${formatNumber(snapshot.cacheCreationInputTokens)}`,
  ].join(' / ')
}

function routeVariant(record: UsageRecord): 'success' | 'secondary' | 'outline' | 'warning' | 'destructive' {
  if (record.routeSubtype === 'external_direct_policy') return 'warning'
  if (record.routeKind === 'external_pool') return record.status === 'success' ? 'success' : 'destructive'
  return 'outline'
}

function upstreamModel(record: UsageRecord): string {
  return record.upstreamModel || record.model || '-'
}

function upstreamModelLabel(record: UsageRecord): string {
  const source = record.modelResolutionSource ? `（${record.modelResolutionSource}）` : ''
  return `${upstreamModel(record)}${source}`
}

function attemptActionLabel(action: string): string {
  switch (action) {
    case 'success':
      return '成功'
    case 'retry':
    case 'transient_retry':
      return '重试'
    case 'fail':
      return '失败'
    case 'disable_and_retry':
      return '禁用后重试'
    case 'failure_count_and_retry':
      return '计失败后重试'
    case 'force_refresh_and_retry':
      return '刷新后重试'
    default:
      return action || '-'
  }
}

function attemptOutcomeLabel(record: NonNullable<UsageRecord['credentialAttempts']>[number]): string {
  if (typeof record.status === 'number') {
    return String(record.status)
  }
  if (record.errorType) {
    return record.errorType
  }
  return attemptActionLabel(record.action)
}

function formatAttemptChain(record: UsageRecord): string {
  const attempts = record.credentialAttempts || []
  return attempts
    .map((attempt) => `#${attempt.credentialId}(${attemptOutcomeLabel(attempt)})`)
    .join(' > ')
}

function formatExternalAttemptChain(record: UsageRecord): string {
  return (record.externalAttempts || [])
    .map((attempt) => `外部池 #${attempt.poolId}(${attempt.status ?? attempt.errorType ?? attempt.action})`)
    .join(' > ')
}

function formatJsonBlock(value: unknown): string {
  if (!value) return '-'
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

export function UsageRecordsPanel() {
  const [searchText, setSearchText] = useState('')
  const [model, setModel] = useState('')
  const [conversationId, setConversationId] = useState('')
  const [credentialId, setCredentialId] = useState('')
  const [status, setStatus] = useState<UsageRecordStatus | ''>('')
  const [source, setSource] = useState<UsageSource | ''>('')
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = useState('')
  const [selectedRecord, setSelectedRecord] = useState<UsageRecord | null>(null)
  const [cleanupOpen, setCleanupOpen] = useState(false)
  const [currentPage, setCurrentPage] = useState(1)
  const itemsPerPage = 20

  const query = useMemo<UsageRecordsPageQuery>(() => {
    const next: UsageRecordsPageQuery = { page: currentPage, limit: itemsPerPage }
    if (searchText.trim()) {
      next.q = searchText.trim()
    }
    if (model.trim()) {
      next.model = model.trim()
    }
    if (conversationId.trim()) {
      next.conversationId = conversationId.trim()
    }
    const parsedCredentialId = Number(credentialId)
    if (credentialId.trim() && Number.isFinite(parsedCredentialId)) {
      next.credentialId = parsedCredentialId
    }
    if (source) {
      next.source = source
    }
    if (status) {
      next.status = status
    }
    if (streamMode !== 'all') {
      next.stream = streamMode === 'stream'
    }
    const parsedMinCacheRead = Number(minCacheRead)
    if (minCacheRead.trim() && Number.isFinite(parsedMinCacheRead)) {
      next.minCacheRead = parsedMinCacheRead
    }
    return next
  }, [conversationId, credentialId, currentPage, minCacheRead, model, searchText, source, status, streamMode])

  const summary = useUsageSummary()
  const records = useUsageRecordsPage(query)
  const modelPricing = useModelPricing()
  const syncPricing = useSyncModelPricing()
  const credentials = useCredentials()
  const clearRecords = useClearUsageRecords()

  useEffect(() => {
    setCurrentPage(1)
  }, [conversationId, credentialId, minCacheRead, model, searchText, source, status, streamMode])

  const credentialLabels = useMemo(() => {
    const labels = new Map<number, string>()
    for (const credential of credentials.data?.credentials || []) {
      labels.set(
        credential.id,
        credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
      )
    }
    return labels
  }, [credentials.data?.credentials])

  const handleRefresh = () => {
    summary.refetch()
    records.refetch()
    modelPricing.refetch()
  }

  const handleSyncPricing = () => {
    syncPricing.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) {
          toast.warning(`价格同步失败，继续使用${status.source === 'built-in' ? '内置价格' : '当前价格'}: ${status.lastError}`)
          return
        }
        toast.success(`价格已同步：${status.modelCount} 个模型`)
        summary.refetch()
        records.refetch()
      },
      onError: (err) => toast.error(`同步失败: ${extractErrorMessage(err)}`),
    })
  }

  const hasFilters = Boolean(
    searchText.trim() ||
    model.trim() ||
    conversationId.trim() ||
    credentialId.trim() ||
    status ||
    source ||
    streamMode !== 'all' ||
    minCacheRead.trim()
  )

  const handleResetFilters = () => {
    setSearchText('')
    setModel('')
    setConversationId('')
    setCredentialId('')
    setStatus('')
    setSource('')
    setStreamMode('all')
    setMinCacheRead('')
  }

  const handleClear = () => {
    if (!confirm('确定清空 Usage 展示记录吗？此操作会在 PgSQL 中软删除当前记录，历史行仍保留用于审计。')) {
      return
    }
    clearRecords.mutate(undefined, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error(`清空失败: ${extractErrorMessage(err)}`),
    })
  }

  const summaryData = summary.data
  const pageRecords = records.data?.records || []
  const hasNextPage = Boolean(records.data?.hasNext)
  const localReadRatio = ratio(
    summaryData?.localPromptCacheReadInputTokens || 0,
    summaryData?.localPromptCacheInputTokens || 0
  )
  const localCachedRatio = ratio(
    (summaryData?.localPromptCacheReadInputTokens || 0) +
      (summaryData?.localPromptCacheCreationInputTokens || 0),
    summaryData?.localPromptCacheInputTokens || 0
  )
  const pricingStatus = modelPricing.data
  const pricedRatio = ratio(summaryData?.pricedRequests || 0, summaryData?.totalRequests || 0)
  const realtime = summaryData?.realtime
  const realtimeWindow = realtime?.windowSeconds || 60

  return (
    <div className="space-y-4">
      <div className="grid gap-4 md:grid-cols-3 xl:grid-cols-6">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">请求总数</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatNumber(summaryData?.totalRequests || 0)}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">实时 RPM</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatNumber(realtime?.rpm || 0)}</div>
            <div className="text-xs text-muted-foreground">
              近 {realtimeWindow} 秒 {formatNumber(realtime?.requests || 0)} 请求
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">实时 TPM</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatNumber(realtime?.totalTpm || 0)}</div>
            <div className="text-xs text-muted-foreground">
              计费 {formatNumber(realtime?.billableTpm || 0)}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">高缓存请求</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold text-green-600">{formatNumber(summaryData?.highCacheRequests || 0)}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">缓存读取</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatNumber(summaryData?.totalCacheReadInputTokens || 0)}</div>
            <div className="text-xs text-muted-foreground">
              本地读取 {formatPercent(localReadRatio)} / 总缓存 {formatPercent(localCachedRatio)}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">估算费用</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatUsd(summaryData?.totalEstimatedCostUsd || 0)}</div>
            <div className="text-xs text-muted-foreground">已计价 {formatPercent(pricedRatio)}</div>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardContent className="flex flex-col gap-3 py-4 md:flex-row md:items-center md:justify-between">
          <div className="space-y-1">
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <span className="font-medium">模型计价</span>
              <Badge variant={pricingStatus?.lastError ? 'warning' : 'secondary'}>
                {pricingStatus?.source || 'loading'}
              </Badge>
              <Badge variant="outline">{formatNumber(pricingStatus?.modelCount || 0)} 个模型</Badge>
              {pricingStatus?.lastSyncedAt && (
                <span className="text-muted-foreground">同步 {formatDate(pricingStatus.lastSyncedAt)}</span>
              )}
            </div>
            <div className="break-all text-xs text-muted-foreground">
              {pricingStatus?.lastError || pricingStatus?.sourceUrl || '正在加载价格目录'}
            </div>
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={handleSyncPricing}
            disabled={syncPricing.isPending}
          >
            <DollarSign className="h-4 w-4" />
            {syncPricing.isPending ? '同步中...' : '同步价格'}
          </Button>
        </CardContent>
      </Card>

      <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 md:flex-row md:items-center md:justify-between">
        <div className="grid flex-1 gap-2 md:grid-cols-2 xl:grid-cols-4">
          <Input
            value={searchText}
            onChange={(event) => setSearchText(event.target.value)}
            placeholder="搜索模型、账号、会话、错误"
            className="xl:col-span-2"
          />
          <Input
            value={model}
            onChange={(event) => setModel(event.target.value)}
            placeholder="模型"
          />
          <Input
            value={conversationId}
            onChange={(event) => setConversationId(event.target.value)}
            placeholder="会话 ID"
          />
          <Input
            value={credentialId}
            onChange={(event) => setCredentialId(event.target.value)}
            placeholder="账号 ID"
            inputMode="numeric"
          />
          <select
            value={status}
            onChange={(event) => setStatus(event.target.value as UsageRecordStatus | '')}
            className="h-10 rounded-md border border-input bg-background px-3 py-2 text-sm"
          >
            <option value="">全部状态</option>
            <option value="success">成功</option>
            <option value="error">错误</option>
            <option value="stream_error">流错误</option>
            <option value="upstream_timeout">上游超时</option>
            <option value="client_dropped">客户端断开</option>
          </select>
          <select
            value={source}
            onChange={(event) => setSource(event.target.value as UsageSource | '')}
            className="h-10 rounded-md border border-input bg-background px-3 py-2 text-sm"
          >
            <option value="">全部来源</option>
            <option value="upstream_metadata">上游 metadata</option>
            <option value="local_prompt_cache">本地 prompt cache</option>
            <option value="context_estimate">上下文估算</option>
            <option value="request_estimate">请求估算</option>
            <option value="none">无缓存</option>
          </select>
          <select
            value={streamMode}
            onChange={(event) => setStreamMode(event.target.value as 'all' | 'stream' | 'non_stream')}
            className="h-10 rounded-md border border-input bg-background px-3 py-2 text-sm"
          >
            <option value="all">全部请求</option>
            <option value="stream">Stream</option>
            <option value="non_stream">非 Stream</option>
          </select>
          <Input
            value={minCacheRead}
            onChange={(event) => setMinCacheRead(event.target.value)}
            placeholder="最小 cache read"
            inputMode="numeric"
          />
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={handleResetFilters} disabled={!hasFilters}>
            <X className="h-4 w-4" />
            重置
          </Button>
          <Button variant="outline" size="sm" onClick={handleRefresh}>
            <RefreshCw className="h-4 w-4" />
            刷新
          </Button>
          <Button variant="outline" size="sm" onClick={() => setCleanupOpen(true)}>
            <Trash2 className="h-4 w-4" />
            分批清理
          </Button>
          <Button
            variant="outline"
            size="sm"
            className="text-destructive hover:text-destructive"
            onClick={handleClear}
            disabled={clearRecords.isPending}
          >
            <Trash2 className="h-4 w-4" />
            清空
          </Button>
        </div>
      </div>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">使用记录</CardTitle>
        </CardHeader>
        <CardContent>
          {records.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : records.error ? (
            <div className="py-8 text-center text-destructive">{extractErrorMessage(records.error)}</div>
          ) : pageRecords.length === 0 && currentPage === 1 ? (
            <div className="py-8 text-center text-muted-foreground">暂无记录</div>
          ) : pageRecords.length === 0 ? (
            <div className="py-8 text-center text-muted-foreground">当前页暂无记录</div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[1720px] text-sm">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">时间</th>
                    <th className="px-3 py-2 font-medium">账号</th>
                    <th className="px-3 py-2 font-medium">模型</th>
                    <th className="px-3 py-2 font-medium">会话</th>
                    <th className="px-3 py-2 font-medium">来源</th>
                    <th className="px-3 py-2 font-medium">状态</th>
                    <th className="px-3 py-2 font-medium text-right">总输入</th>
                    <th className="px-3 py-2 font-medium text-right">上报输入</th>
                    <th className="px-3 py-2 font-medium text-right">计费输入</th>
                    <th className="px-3 py-2 font-medium text-right">缓存读取</th>
                    <th className="px-3 py-2 font-medium text-right">缓存写入</th>
                    <th className="px-3 py-2 font-medium text-right">读取率</th>
                    <th className="px-3 py-2 font-medium text-right">缓存率</th>
                    <th className="px-3 py-2 font-medium text-right">输出</th>
                    <th className="px-3 py-2 font-medium text-right">费用</th>
                    <th className="px-3 py-2 font-medium text-right">耗时</th>
                  </tr>
                </thead>
                <tbody>
                  {pageRecords.map((record) => {
                    const credentialLabel =
                      typeof record.credentialId === 'number'
                        ? credentialLabels.get(record.credentialId) || record.credentialLabel
                        : record.credentialLabel
                    const readRatio = ratio(record.cacheReadInputTokens, record.totalInputTokens)
                    const cachedRatio = ratio(
                      record.cacheReadInputTokens + record.cacheCreationInputTokens,
                      record.totalInputTokens
                    )
                    const attemptChain = formatAttemptChain(record)
                    const externalAttemptChain = formatExternalAttemptChain(record)
                    const isExternal = record.routeKind === 'external_pool'

                    return (
                    <tr key={record.id} className="border-b last:border-0">
                      <td className="px-3 py-2 whitespace-nowrap">{formatDate(record.createdAt)}</td>
                      <td className="px-3 py-2">
                        <div className="font-medium">
                          {isExternal ? `外部池 #${record.externalPoolId ?? '-'}` : `#${record.credentialId ?? '-'}`}
                        </div>
                        {credentialLabel && (
                          <div className="max-w-[240px] truncate text-xs text-muted-foreground" title={credentialLabel}>
                            {credentialLabel}
                          </div>
                        )}
                        {isExternal && record.externalPoolName && (
                          <div className="max-w-[240px] truncate text-xs text-muted-foreground" title={record.externalPoolName}>
                            {record.externalPoolName}
                          </div>
                        )}
                        {attemptChain && (
                          <button
                            type="button"
                            className="mt-1 block max-w-[260px] truncate text-left text-xs text-muted-foreground underline-offset-2 hover:underline"
                            onClick={() => setSelectedRecord(record)}
                            title={attemptChain}
                          >
                            链路 {attemptChain}
                          </button>
                        )}
                        {externalAttemptChain && (
                          <button
                            type="button"
                            className="mt-1 block max-w-[260px] truncate text-left text-xs text-muted-foreground underline-offset-2 hover:underline"
                            onClick={() => setSelectedRecord(record)}
                            title={externalAttemptChain}
                          >
                            外部 {externalAttemptChain}
                          </button>
                        )}
                      </td>
                      <td className="px-3 py-2">
                        <div className="max-w-[260px] truncate font-medium" title={record.model || '-'}>
                          请求 {record.model || '-'}
                        </div>
                        <div className="max-w-[260px] truncate text-xs text-muted-foreground" title={upstreamModelLabel(record)}>
                          上游 {upstreamModelLabel(record)}
                        </div>
                        <div className="mt-1 flex flex-wrap gap-1">
                          <Badge variant="outline">{record.endpoint || '-'}</Badge>
                          {record.stream ? <Badge variant="secondary">stream</Badge> : <Badge variant="outline">non-stream</Badge>}
                        </div>
                      </td>
                      <td className="px-3 py-2">
                        <div className="max-w-[220px] truncate">{record.conversationId || '-'}</div>
                        <div className="mt-1 flex flex-wrap gap-1">
                          {record.stickyBound && <Badge variant="secondary">sticky</Badge>}
                          {record.fallbackFromSticky && <Badge variant="warning">fallback</Badge>}
                        </div>
                      </td>
                      <td className="px-3 py-2">
                        <Badge variant={record.simulated ? 'warning' : 'secondary'}>
                          {sourceLabel(record.usageSource)}
                        </Badge>
                        <div className="mt-1">
                          <Badge variant={routeVariant(record)} title={record.routeSubtype || record.routeKind || ''}>
                            {routeLabel(record)}
                          </Badge>
                        </div>
                      </td>
                      <td className="px-3 py-2">
                        <Badge variant={statusVariant(record.status)} title={record.status}>
                          {statusLabel(record.status)}
                        </Badge>
                        {record.errorMessage && (
                          <button
                            type="button"
                            className="mt-1 block max-w-[220px] truncate text-left text-xs text-muted-foreground underline-offset-2 hover:underline"
                            onClick={() => setSelectedRecord(record)}
                            title={record.errorDetail || record.errorMessage}
                          >
                            {record.errorMessage}
                          </button>
                        )}
                      </td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.totalInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.compatInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.billableInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.cacheReadInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.cacheCreationInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatPercent(readRatio)}</td>
                      <td className="px-3 py-2 text-right">{formatPercent(cachedRatio)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.outputTokens)}</td>
                      <td className="px-3 py-2 text-right">
                        <div>{formatUsd(record.estimatedCostUsd || 0)}</div>
                        <div className="text-xs text-muted-foreground">
                          {record.pricingAvailable ? record.pricingModel || 'priced' : 'unpriced'}
                        </div>
                      </td>
                      <td className="px-3 py-2 text-right">
                        <div className="flex items-center justify-end gap-2">
                          <span>{formatNumber(record.durationMs)}ms</span>
                          {(record.errorMessage || record.errorDetail || attemptChain) && (
                            <Button
                              variant="ghost"
                              size="icon"
                              className="h-7 w-7"
                              onClick={() => setSelectedRecord(record)}
                              title="查看详情"
                            >
                              <Eye className="h-4 w-4" />
                            </Button>
                          )}
                        </div>
                      </td>
                    </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
          {(currentPage > 1 || hasNextPage) && (
            <div className="mt-4 flex items-center justify-center gap-4">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                disabled={currentPage === 1}
              >
                上一页
              </Button>
              <span className="text-sm text-muted-foreground">
                第 {currentPage} 页，每页 {itemsPerPage} 条
              </span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setCurrentPage(p => p + 1)}
                disabled={!hasNextPage}
              >
                下一页
              </Button>
            </div>
          )}
        </CardContent>
      </Card>

      <Dialog open={Boolean(selectedRecord)} onOpenChange={(open) => !open && setSelectedRecord(null)}>
        <DialogContent className="max-h-[85vh] max-w-4xl overflow-y-auto">
          <DialogHeader>
            <DialogTitle>使用详情</DialogTitle>
          </DialogHeader>
          {selectedRecord && (
            <div className="space-y-4">
              <div className="grid gap-3 text-sm md:grid-cols-2">
                <div>
                  <div className="text-xs text-muted-foreground">请求 ID</div>
                  <div className="break-all font-mono">{selectedRecord.id}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">时间</div>
                  <div>{formatDate(selectedRecord.createdAt)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">请求模型</div>
                  <div className="break-all">{selectedRecord.model || '-'}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">上游模型</div>
                  <div className="break-all">{upstreamModel(selectedRecord)}</div>
                  <div className="mt-1 text-xs text-muted-foreground">
                    解析来源：{selectedRecord.modelResolutionSource || '-'}
                  </div>
                  {selectedRecord.modelResolutionNote && (
                    <div className="mt-1 break-all text-xs text-muted-foreground">
                      {selectedRecord.modelResolutionNote}
                    </div>
                  )}
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">会话</div>
                  <div className="break-all">{selectedRecord.conversationId || '-'}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">账号</div>
                  <div>
                    #{selectedRecord.credentialId ?? '-'} {selectedRecord.credentialLabel || ''}
                  </div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">路由</div>
                  <div>{routeLabel(selectedRecord)}</div>
                  <div className="mt-1 text-xs text-muted-foreground">
                    {selectedRecord.routeKind || '-'} {selectedRecord.routeSubtype ? `· ${selectedRecord.routeSubtype}` : ''}
                  </div>
                </div>
                {selectedRecord.routeKind === 'external_pool' && (
                  <div>
                    <div className="text-xs text-muted-foreground">外部池</div>
                    <div>
                      #{selectedRecord.externalPoolId ?? '-'} {selectedRecord.externalPoolName || ''}
                    </div>
                  </div>
                )}
                {(selectedRecord.fallbackReason || selectedRecord.directPolicyReason) && (
                  <div>
                    <div className="text-xs text-muted-foreground">路由原因</div>
                    <div className="break-all">
                      {selectedRecord.fallbackReason || selectedRecord.directPolicyReason}
                    </div>
                  </div>
                )}
                <div>
                  <div className="text-xs text-muted-foreground">状态</div>
                  <div>{statusLabel(selectedRecord.status)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">估算费用</div>
                  <div>
                    {formatUsd(selectedRecord.estimatedCostUsd || 0)}
                    <span className="ml-2 text-xs text-muted-foreground">
                      {selectedRecord.pricingAvailable
                        ? selectedRecord.pricingModel || 'priced'
                        : 'unpriced'}
                    </span>
                  </div>
                </div>
              </div>
              {selectedRecord.externalPoolBilling && (
                <div className="rounded-md border bg-muted/30 p-3 text-sm">
                  <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
                    <div className="font-medium">外部池成本保护</div>
                    <Badge variant={selectedRecord.externalPoolBilling.costFloorApplied ? 'warning' : 'success'}>
                      {selectedRecord.externalPoolBilling.costFloorApplied ? '已保底补差' : '未触发补差'}
                    </Badge>
                  </div>
                  <div className="grid gap-3 md:grid-cols-2">
                    <div>
                      <div className="text-xs text-muted-foreground">渠道 raw usage / 成本</div>
                      <div className="break-all font-mono text-xs">{formatUsageSnapshot(selectedRecord.externalPoolBilling.rawUsage)}</div>
                      <div className="mt-1 font-medium">{formatUsd(selectedRecord.externalPoolBilling.rawCostUsd || 0)}</div>
                    </div>
                    <div>
                      <div className="text-xs text-muted-foreground">返回 reported usage / 成本</div>
                      <div className="break-all font-mono text-xs">{formatUsageSnapshot(selectedRecord.externalPoolBilling.reportedUsage)}</div>
                      <div className="mt-1 font-medium">{formatUsd(selectedRecord.externalPoolBilling.reportedCostUsd || 0)}</div>
                    </div>
                    <div>
                      <div className="text-xs text-muted-foreground">最终计费 / 补差</div>
                      <div className="font-medium">{formatUsd(selectedRecord.externalPoolBilling.billableCostUsd || 0)}</div>
                      <div className="text-xs text-muted-foreground">补差 {formatUsd(selectedRecord.externalPoolBilling.costFloorDeltaUsd || 0)}</div>
                    </div>
                    <div>
                      <div className="text-xs text-muted-foreground">计价模型 / 整形模式</div>
                      <div className="break-all">{selectedRecord.externalPoolBilling.pricingAvailable ? selectedRecord.externalPoolBilling.pricingModel || 'priced' : 'unpriced'}</div>
                      <div className="text-xs text-muted-foreground">{selectedRecord.externalPoolBilling.usageProjectionMode}</div>
                    </div>
                  </div>
                </div>
              )}
              {(selectedRecord.credentialAttempts || []).length > 0 && (
                <div>
                  <div className="mb-2 text-sm font-medium">调用链路</div>
                  <div className="mb-2 rounded-md border bg-muted px-3 py-2 font-mono text-xs">
                    {formatAttemptChain(selectedRecord)}
                  </div>
                  <div className="overflow-x-auto rounded-md border">
                    <table className="w-full min-w-[760px] text-xs">
                      <thead className="bg-muted text-muted-foreground">
                        <tr className="text-left">
                          <th className="px-3 py-2 font-medium">顺序</th>
                          <th className="px-3 py-2 font-medium">账号</th>
                          <th className="px-3 py-2 font-medium">状态</th>
                          <th className="px-3 py-2 font-medium">动作</th>
                          <th className="px-3 py-2 font-medium text-right">耗时</th>
                          <th className="px-3 py-2 font-medium">错误</th>
                        </tr>
                      </thead>
                      <tbody>
                        {(selectedRecord.credentialAttempts || []).map((attempt) => (
                          <tr key={`${attempt.attempt}-${attempt.credentialId}-${attempt.durationMs}`} className="border-t">
                            <td className="px-3 py-2">{attempt.attempt}</td>
                            <td className="px-3 py-2">
                              <div className="font-medium">#{attempt.credentialId}</div>
                              {attempt.credentialLabel && (
                                <div className="max-w-[220px] truncate text-muted-foreground" title={attempt.credentialLabel}>
                                  {attempt.credentialLabel}
                                </div>
                              )}
                              {attempt.model && (
                                <div className="max-w-[220px] truncate text-xs text-muted-foreground" title={attempt.model}>
                                  模型 {attempt.model}
                                </div>
                              )}
                            </td>
                            <td className="px-3 py-2">{attempt.statusText || attempt.status || '-'}</td>
                            <td className="px-3 py-2">{attemptActionLabel(attempt.action)}</td>
                            <td className="px-3 py-2 text-right">{formatNumber(attempt.durationMs)}ms</td>
                            <td className="px-3 py-2">
                              <div className="max-w-[280px] truncate" title={attempt.errorMessage || attempt.errorType || ''}>
                                {attempt.errorMessage || attempt.errorType || '-'}
                              </div>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              )}
              {(selectedRecord.externalAttempts || []).length > 0 && (
                <div>
                  <div className="mb-2 text-sm font-medium">外部池链路</div>
                  <div className="mb-2 rounded-md border bg-muted px-3 py-2 font-mono text-xs">
                    {formatExternalAttemptChain(selectedRecord)}
                  </div>
                  <div className="overflow-x-auto rounded-md border">
                    <table className="w-full min-w-[760px] text-xs">
                      <thead className="bg-muted text-muted-foreground">
                        <tr className="text-left">
                          <th className="px-3 py-2 font-medium">顺序</th>
                          <th className="px-3 py-2 font-medium">外部池</th>
                          <th className="px-3 py-2 font-medium">状态</th>
                          <th className="px-3 py-2 font-medium">动作</th>
                          <th className="px-3 py-2 font-medium text-right">耗时</th>
                          <th className="px-3 py-2 font-medium">错误</th>
                        </tr>
                      </thead>
                      <tbody>
                        {(selectedRecord.externalAttempts || []).map((attempt) => (
                          <tr key={`${attempt.attempt}-${attempt.poolId}-${attempt.durationMs}`} className="border-t">
                            <td className="px-3 py-2">{attempt.attempt}</td>
                            <td className="px-3 py-2">
                              <div className="font-medium">#{attempt.poolId}</div>
                              <div className="max-w-[220px] truncate text-muted-foreground" title={attempt.poolName}>
                                {attempt.poolName}
                              </div>
                            </td>
                            <td className="px-3 py-2">{attempt.status || '-'}</td>
                            <td className="px-3 py-2">{attemptActionLabel(attempt.action)}</td>
                            <td className="px-3 py-2 text-right">{formatNumber(attempt.durationMs)}ms</td>
                            <td className="px-3 py-2">
                              <div className="max-w-[280px] truncate" title={attempt.errorMessage || attempt.errorType || ''}>
                                {attempt.errorMessage || attempt.errorType || '-'}
                              </div>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              )}
              <div>
                <div className="mb-2 text-sm font-medium">错误详情</div>
                <pre className="max-h-[360px] overflow-auto rounded-md border bg-muted p-3 text-xs whitespace-pre-wrap break-words">
                  {selectedRecord.errorDetail || selectedRecord.errorMessage || '-'}
                </pre>
              </div>
              {Boolean(selectedRecord.payloadBreakdown || selectedRecord.payloadGuardReport) && (
                <div>
                  <div className="mb-2 text-sm font-medium">Payload 诊断</div>
                  <pre className="max-h-[360px] overflow-auto rounded-md border bg-muted p-3 text-xs whitespace-pre-wrap break-words">
                    {formatJsonBlock({
                      breakdown: selectedRecord.payloadBreakdown || null,
                      guard: selectedRecord.payloadGuardReport || null,
                    })}
                  </pre>
                </div>
              )}
            </div>
          )}
        </DialogContent>
      </Dialog>

      <UsageCleanupDialog open={cleanupOpen} onOpenChange={setCleanupOpen} />
    </div>
  )
}

function cleanupModeLabel(mode?: UsageCleanupMode): string {
  return mode === 'hard_delete' ? '硬删除已软删记录' : '软删除可见明细'
}

function cleanupStatusLabel(status?: string): string {
  switch (status) {
    case 'running':
      return '运行中'
    case 'completed':
      return '已完成'
    case 'cancelled':
      return '已取消'
    case 'failed':
      return '失败'
    default:
      return '空闲'
  }
}

const USAGE_CLEANUP_DEFAULT_MAX_BATCHES = 10000

function parseCleanupInteger(value: string, fallback: number, min: number): number {
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.max(min, Math.floor(parsed))
}

function UsageCleanupDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const [mode, setMode] = useState<UsageCleanupMode>('soft_delete')
  const [olderThanDays, setOlderThanDays] = useState('7')
  const [batchSize, setBatchSize] = useState('1000')
  const [pauseMs, setPauseMs] = useState('100')
  const cleanupStatus = useUsageCleanupStatus()
  const previewCleanup = usePreviewUsageCleanup()
  const startCleanup = useStartUsageCleanup()
  const cancelCleanup = useCancelUsageCleanup()

  const parsedOlderThanDays = parseCleanupInteger(olderThanDays, 7, 1)
  const parsedBatchSize = parseCleanupInteger(batchSize, 1000, 1)
  const parsedPauseMs = parseCleanupInteger(pauseMs, 100, 0)
  const payload = (): UsageCleanupRequest => ({
    mode,
    olderThanDays: parsedOlderThanDays,
    batchSize: parsedBatchSize,
    pauseMsBetweenBatches: parsedPauseMs,
  })

  const running = cleanupStatus.data?.status === 'running'
  const preview = previewCleanup.data
  const estimatedBatches = preview
    ? Math.ceil(preview.matchedRows / Math.max(parsedBatchSize, 1))
    : null

  const previewRows = () => {
    previewCleanup.mutate(payload(), {
      onError: (error) => toast.error(`预估失败: ${extractErrorMessage(error)}`),
    })
  }

  const start = () => {
    const cutoffLabel = mode === 'hard_delete' ? '删除时间' : '创建时间'
    const confirmed = confirm(
      `确定开始${cleanupModeLabel(mode)}？\n\n范围：${cutoffLabel}早于 ${parsedOlderThanDays} 天\n每批：${formatNumber(parsedBatchSize)} 条\n系统会持续分批执行，直到没有更多匹配记录或达到内部安全上限 ${formatNumber(USAGE_CLEANUP_DEFAULT_MAX_BATCHES)} 批。\n\n清理只影响使用记录明细列表，已累计的顶部统计和 Dashboard rollup 会保留。`
    )
    if (!confirmed) return

    startCleanup.mutate(payload(), {
      onSuccess: () => {
        toast.success('Usage 分批清理已启动')
        cleanupStatus.refetch()
      },
      onError: (error) => toast.error(`启动失败: ${extractErrorMessage(error)}`),
    })
  }

  const cancel = () => {
    cancelCleanup.mutate(undefined, {
      onSuccess: () => {
        toast.info('已请求取消清理任务')
        cleanupStatus.refetch()
      },
      onError: (error) => toast.error(`取消失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>分批清理 Usage 记录</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 text-sm">
          <div className="rounded-md border border-yellow-500/30 bg-yellow-500/10 p-3 text-yellow-700 dark:text-yellow-300">
            这是手动单次任务，不会定时执行。你只需要设置清理范围和每批数量，系统会自动分批清到没有更多匹配记录；后端保留 {formatNumber(USAGE_CLEANUP_DEFAULT_MAX_BATCHES)} 批安全上限。清理只影响使用记录明细列表，已累计的顶部统计和 Dashboard rollup 会保留。
          </div>

          <div className="grid gap-3 md:grid-cols-2">
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">清理方式</span>
              <select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" value={mode} onChange={(event) => setMode(event.target.value as UsageCleanupMode)}>
                <option value="soft_delete">软删除可见明细</option>
                <option value="hard_delete">硬删除已软删记录</option>
              </select>
            </label>
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">{mode === 'hard_delete' ? '删除时间早于多少天' : '创建时间早于多少天'}</span>
              <Input value={olderThanDays} onChange={(event) => setOlderThanDays(event.target.value)} inputMode="numeric" />
            </label>
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">每批数量</span>
              <Input value={batchSize} onChange={(event) => setBatchSize(event.target.value)} inputMode="numeric" />
            </label>
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">批次间隔毫秒</span>
              <Input value={pauseMs} onChange={(event) => setPauseMs(event.target.value)} inputMode="numeric" />
            </label>
          </div>

          {preview && (
            <div className="rounded-md border bg-muted/30 p-3">
              <div className="font-medium">预估：{cleanupModeLabel(preview.mode)}，匹配 {formatNumber(preview.matchedRows)} 条</div>
              <div className="mt-1 text-xs text-muted-foreground">
                cutoff {formatDate(preview.cutoffAt)} · 预计 {formatNumber(estimatedBatches || 0)} 批 · 匹配记录创建时间 {formatDate(preview.oldestCreatedAt)} 至 {formatDate(preview.newestCreatedAt)}
              </div>
            </div>
          )}

          <div className="rounded-md border bg-muted/30 p-3">
            <div className="font-medium">当前任务：{cleanupStatusLabel(cleanupStatus.data?.status)}</div>
            {cleanupStatus.data?.jobId && (
              <div className="mt-1 grid gap-1 text-xs text-muted-foreground md:grid-cols-2">
                <span>任务 {cleanupStatus.data.jobId}</span>
                <span>模式 {cleanupModeLabel(cleanupStatus.data.mode)}</span>
                <span>已处理 {formatNumber(cleanupStatus.data.processedRows)} 条</span>
                <span>剩余约 {formatNumber(cleanupStatus.data.remainingRows || 0)} 条</span>
                <span>已执行 {formatNumber(cleanupStatus.data.batches)} 批</span>
                <span>内部安全上限 {formatNumber(cleanupStatus.data.maxBatches)} 批</span>
                <span>最后一批 {formatNumber(cleanupStatus.data.lastBatchRows)} 条</span>
                {cleanupStatus.data.stopReason && <span>停止原因 {cleanupStatus.data.stopReason}</span>}
                {cleanupStatus.data.lastError && <span className="text-destructive">错误 {cleanupStatus.data.lastError}</span>}
              </div>
            )}
          </div>

          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="outline" onClick={previewRows} disabled={previewCleanup.isPending || running}>
              {previewCleanup.isPending ? '预估中...' : '预估'}
            </Button>
            <Button onClick={start} disabled={startCleanup.isPending || running}>
              {startCleanup.isPending ? '启动中...' : '开始分批清理'}
            </Button>
            <Button variant="outline" onClick={cancel} disabled={!running || cancelCleanup.isPending}>
              请求取消
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
