import { useEffect, useMemo, useState } from 'react'
import { RefreshCw, Trash2, X } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { useCredentials } from '@/hooks/use-credentials'
import { useClearUsageRecords, useUsageRecordsPage, useUsageSummary } from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
import type { UsageRecord, UsageRecordsPageQuery, UsageRecordStatus, UsageSource } from '@/types/api'

function formatNumber(value: number): string {
  return new Intl.NumberFormat('zh-CN').format(value)
}

function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return `${(value * 100).toFixed(1)}%`
}

function ratio(part: number, total: number): number {
  if (!Number.isFinite(part) || !Number.isFinite(total) || total <= 0) {
    return Number.NaN
  }
  return part / total
}

function formatDate(value: string): string {
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

function uniqueIds(ids: number[] | undefined): number[] {
  return Array.from(new Set(ids || []))
}

function traceTitle(record: UsageRecord): string {
  const parts: string[] = []
  const attempts = record.attemptedCredentialIds || []
  const rateLimited = uniqueIds(record.rateLimitedCredentialIds)
  if (attempts.length > 0) {
    parts.push(`尝试链路: ${attempts.map((id) => `#${id}`).join(' -> ')}`)
  }
  if (rateLimited.length > 0) {
    parts.push(`429账号: ${rateLimited.map((id) => `#${id}`).join(', ')}`)
  }
  if (record.schedulerBlocked) {
    parts.push('调度阶段被全池退避/冷却拦截')
  }
  return parts.join('\n')
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
  const [currentPage, setCurrentPage] = useState(1)
  const itemsPerPage = 100

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
  const credentials = useCredentials()
  const clearRecords = useClearUsageRecords()

  useEffect(() => {
    setCurrentPage(1)
  }, [conversationId, credentialId, minCacheRead, model, searchText, source, status, streamMode])

  useEffect(() => {
    if (!records.data) {
      return
    }

    const nextPage = records.data.totalPages > 0 ? Math.min(currentPage, records.data.totalPages) : 1
    if (currentPage !== nextPage) {
      setCurrentPage(nextPage)
    }
  }, [currentPage, records.data])

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
    if (!confirm('确定清空 usage 记录吗？此操作会同时截断本地 JSONL 记录文件。')) {
      return
    }
    clearRecords.mutate(undefined, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error(`清空失败: ${extractErrorMessage(err)}`),
    })
  }

  const summaryData = summary.data
  const totalPages = records.data?.totalPages || 0
  const totalRecords = records.data?.total || 0
  const localReadRatio = ratio(
    summaryData?.localPromptCacheReadInputTokens || 0,
    summaryData?.localPromptCacheInputTokens || 0
  )
  const localCachedRatio = ratio(
    (summaryData?.localPromptCacheReadInputTokens || 0) +
      (summaryData?.localPromptCacheCreationInputTokens || 0),
    summaryData?.localPromptCacheInputTokens || 0
  )

  return (
    <div className="space-y-4">
      <div className="grid gap-4 md:grid-cols-4">
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
            <CardTitle className="text-sm font-medium text-muted-foreground">高缓存请求</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold text-green-600">{formatNumber(summaryData?.highCacheRequests || 0)}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Cache Read</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatNumber(summaryData?.totalCacheReadInputTokens || 0)}</div>
            <div className="text-xs text-muted-foreground">local read {formatPercent(localReadRatio)}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Local Cached</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatPercent(localCachedRatio)}</div>
            <div className="text-xs text-muted-foreground">
              local {formatNumber(summaryData?.localPromptCacheRequests || 0)}
            </div>
          </CardContent>
        </Card>
      </div>

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
          <CardTitle className="text-base">Usage 记录</CardTitle>
        </CardHeader>
        <CardContent>
          {records.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : records.error ? (
            <div className="py-8 text-center text-destructive">{extractErrorMessage(records.error)}</div>
          ) : totalRecords === 0 ? (
            <div className="py-8 text-center text-muted-foreground">暂无记录</div>
          ) : records.data?.records.length === 0 ? (
            <div className="py-8 text-center text-muted-foreground">当前页暂无记录</div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[1600px] text-sm">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">时间</th>
                    <th className="px-3 py-2 font-medium">账号</th>
                    <th className="px-3 py-2 font-medium">模型</th>
                    <th className="px-3 py-2 font-medium">会话</th>
                    <th className="px-3 py-2 font-medium">来源</th>
                    <th className="px-3 py-2 font-medium">状态</th>
                    <th className="px-3 py-2 font-medium text-right">Total In</th>
                    <th className="px-3 py-2 font-medium text-right">Compat In</th>
                    <th className="px-3 py-2 font-medium text-right">Billable In</th>
                    <th className="px-3 py-2 font-medium text-right">Cache Read</th>
                    <th className="px-3 py-2 font-medium text-right">Cache Create</th>
                    <th className="px-3 py-2 font-medium text-right">Read %</th>
                    <th className="px-3 py-2 font-medium text-right">Cached %</th>
                    <th className="px-3 py-2 font-medium text-right">输出</th>
                    <th className="px-3 py-2 font-medium text-right">耗时</th>
                  </tr>
                </thead>
                <tbody>
                  {records.data?.records.map((record) => {
                    const primaryCredentialId = record.credentialId ?? record.lastAttemptedCredentialId
                    const credentialLabel =
                      typeof primaryCredentialId === 'number'
                        ? credentialLabels.get(primaryCredentialId) || record.credentialLabel
                        : record.credentialLabel
                    const attempts = record.attemptedCredentialIds || []
                    const rateLimited = uniqueIds(record.rateLimitedCredentialIds)
                    const trace = traceTitle(record)
                    const readRatio = ratio(record.cacheReadInputTokens, record.totalInputTokens)
                    const cachedRatio = ratio(
                      record.cacheReadInputTokens + record.cacheCreationInputTokens,
                      record.totalInputTokens
                    )

                    return (
                    <tr key={record.id} className="border-b last:border-0">
                      <td className="px-3 py-2 whitespace-nowrap">{formatDate(record.createdAt)}</td>
                      <td className="px-3 py-2">
                        <div className="font-medium">#{primaryCredentialId ?? '-'}</div>
                        {credentialLabel && (
                          <div className="max-w-[240px] truncate text-xs text-muted-foreground" title={credentialLabel}>
                            {credentialLabel}
                          </div>
                        )}
                        {trace && (
                          <div className="mt-1 max-w-[240px] truncate text-xs text-muted-foreground" title={trace}>
                            {attempts.length > 0 && `尝试 ${attempts.map((id) => `#${id}`).join(' -> ')}`}
                            {attempts.length === 0 && rateLimited.length > 0 && `429 ${rateLimited.map((id) => `#${id}`).join(', ')}`}
                          </div>
                        )}
                      </td>
                      <td className="px-3 py-2">
                        <div className="max-w-[260px] truncate font-medium" title={record.model}>
                          {record.model || '-'}
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
                      </td>
                      <td className="px-3 py-2">
                        <Badge variant={statusVariant(record.status)} title={record.status}>
                          {statusLabel(record.status)}
                        </Badge>
                        {record.errorMessage && (
                          <div className="mt-1 max-w-[220px] truncate text-xs text-muted-foreground">
                            {record.errorMessage}
                          </div>
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
                      <td className="px-3 py-2 text-right">{formatNumber(record.durationMs)}ms</td>
                    </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
          {totalPages > 1 && (
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
                第 {currentPage} / {totalPages} 页（共 {totalRecords} 条记录）
              </span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                disabled={currentPage === totalPages}
              >
                下一页
              </Button>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
