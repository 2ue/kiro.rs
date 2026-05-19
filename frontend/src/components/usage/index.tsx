import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Filter, RefreshCw, Search, Trash2, X } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Pagination } from '@/components/ui/pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from '@/components/ui/sheet'
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useCredentialsList } from '@/hooks/use-credentials'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import {
  useClearUsageRecords,
  useUsageRecordsPage,
  useUsageStats,
  useUsageSummary,
} from '@/hooks/use-usage'
import { usageSourceLabel, usageStatusLabel } from '@/lib/pricing'
import {
  extractErrorMessage,
  formatDateTime,
  formatNumber,
  formatPercent,
  formatUsd,
} from '@/lib/utils'
import { usePreferences } from '@/store/preferences'
import type {
  UsageRecord,
  UsageRecordStatus,
  UsageRecordsPageQuery,
  UsageRecordsQuery,
  UsageSource,
} from '@/types/api'

const PAGE_SIZE_OPTIONS = [20, 50, 100, 200, 500]

const STATUS_OPTIONS: Array<{ value: UsageRecordStatus | 'all'; label: string }> = [
  { value: 'all', label: '全部状态' },
  { value: 'success', label: '成功' },
  { value: 'error', label: '错误' },
  { value: 'stream_error', label: '流错误' },
  { value: 'upstream_timeout', label: '上游超时' },
  { value: 'client_dropped', label: '客户端断开' },
]

const SOURCE_OPTIONS: Array<{ value: UsageSource | 'all'; label: string; tip: string }> = [
  { value: 'all', label: '全部来源', tip: '不限' },
  {
    value: 'upstream_metadata',
    label: '上游真实',
    tip: '从 Kiro 上游 metadata 直接获取的可信回执',
  },
  {
    value: 'local_prompt_cache',
    label: '本地缓存推算',
    tip: '本地基于会话维度推算出的 cache 模拟值',
  },
  {
    value: 'context_estimate',
    label: '上下文估算',
    tip: '上游未给 metadata,用上下文 token 估算',
  },
  {
    value: 'request_estimate',
    label: '请求估算',
    tip: '彻底估算请求体的 token 数,精度最低',
  },
  { value: 'none', label: '无缓存数据', tip: '没有任何缓存信息' },
]

function ratio(part: number, total: number): number {
  if (!Number.isFinite(part) || !Number.isFinite(total) || total <= 0) {
    return Number.NaN
  }
  return part / total
}

function uniqueIds(ids: number[] | undefined): number[] {
  return Array.from(new Set(ids || []))
}

function credentialTraceTitle(record: UsageRecord): string {
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

export default function UsagePage() {
  const pageSize = usePreferences((s) => s.pageSize)
  const setPageSize = usePreferences((s) => s.setPageSize)

  const [page, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [filtersOpen, setFiltersOpen] = useState(false)

  const [model, setModel] = useState('')
  const [conversationId, setConversationId] = useState('')
  const [credentialId, setCredentialId] = useState('')
  const [status, setStatus] = useState<UsageRecordStatus | 'all'>('all')
  const [source, setSource] = useState<UsageSource | 'all'>('all')
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = useState('')

  const debouncedSearch = useDebouncedValue(search, 400)
  const debouncedModel = useDebouncedValue(model, 400)
  const debouncedConversation = useDebouncedValue(conversationId, 400)
  const debouncedCredentialId = useDebouncedValue(credentialId, 400)
  const debouncedMinCacheRead = useDebouncedValue(minCacheRead, 400)

  useEffect(() => {
    setPage(1)
  }, [
    debouncedSearch,
    debouncedModel,
    debouncedConversation,
    debouncedCredentialId,
    status,
    source,
    streamMode,
    debouncedMinCacheRead,
    pageSize,
  ])

  // 共享的过滤条件(列表 + 统计共用,保证口径一致)
  const filter = useMemo<UsageRecordsQuery>(() => {
    const next: UsageRecordsQuery = {}
    if (debouncedSearch.trim()) next.q = debouncedSearch.trim()
    if (debouncedModel.trim()) next.model = debouncedModel.trim()
    if (debouncedConversation.trim()) next.conversationId = debouncedConversation.trim()
    const credId = Number(debouncedCredentialId)
    if (debouncedCredentialId.trim() && Number.isFinite(credId)) {
      next.credentialId = credId
    }
    if (status !== 'all') next.status = status
    if (source !== 'all') next.source = source
    if (streamMode !== 'all') next.stream = streamMode === 'stream'
    const minCache = Number(debouncedMinCacheRead)
    if (debouncedMinCacheRead.trim() && Number.isFinite(minCache)) {
      next.minCacheRead = minCache
    }
    return next
  }, [
    debouncedSearch,
    debouncedModel,
    debouncedConversation,
    debouncedCredentialId,
    status,
    source,
    streamMode,
    debouncedMinCacheRead,
  ])

  const pageQuery = useMemo<UsageRecordsPageQuery>(
    () => ({ ...filter, page, limit: pageSize }),
    [filter, page, pageSize],
  )

  const summaryQuery = useUsageSummary()
  const statsQuery = useUsageStats(filter)
  const recordsQuery = useUsageRecordsPage(pageQuery)
  const credentialsQuery = useCredentialsList()
  const clearMutation = useClearUsageRecords()

  const credentialLabels = useMemo(() => {
    const map = new Map<number, string>()
    for (const c of credentialsQuery.data?.credentials ?? []) {
      map.set(c.id, c.email || c.maskedApiKey || `凭据 #${c.id}`)
    }
    return map
  }, [credentialsQuery.data?.credentials])

  const summary = summaryQuery.data
  const stats = statsQuery.data
  const totalPages = recordsQuery.data?.totalPages ?? 0

  const hasFilters =
    !!filter.q ||
    !!filter.model ||
    !!filter.conversationId ||
    typeof filter.credentialId === 'number' ||
    !!filter.status ||
    !!filter.source ||
    typeof filter.stream === 'boolean' ||
    typeof filter.minCacheRead === 'number'

  const resetFilters = () => {
    setSearch('')
    setModel('')
    setConversationId('')
    setCredentialId('')
    setStatus('all')
    setSource('all')
    setStreamMode('all')
    setMinCacheRead('')
  }

  const handleClear = () => {
    if (!confirm('清空所有用量记录?该操作会同时截断本地 JSONL 文件,无法撤销。')) return
    clearMutation.mutate(undefined, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error(extractErrorMessage(err)),
    })
  }

  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">用量分析</h1>
        <p className="text-sm text-muted-foreground">
          请求历史 / 模型分布 / token 与成本明细。**统计与筛选条件保持一致,与分页无关**。
        </p>
      </div>

      {/* 顶部统计 — 来自 SQL 聚合,受 filter 影响,不受分页影响 */}
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              {hasFilters ? '筛选请求' : '请求总数'}
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {formatNumber(stats?.totalRequests ?? 0)}
            </div>
            <div className="text-xs text-muted-foreground">
              今日 {formatNumber(stats?.todayRequests ?? 0)}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              成功率
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {summary
                ? formatPercent(
                    ratio(summary.successRequests, summary.totalRequests),
                    1,
                  )
                : '—'}
            </div>
            <div className="text-xs text-muted-foreground">
              成功 {formatNumber(summary?.successRequests ?? 0)} ·
              失败 {formatNumber(summary?.errorRequests ?? 0)}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              输入 / 输出 token
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {formatNumber(stats?.totalTokens ?? 0)}
            </div>
            <div className="text-xs text-muted-foreground">
              输出 {formatNumber(stats?.totalOutputTokens ?? 0)}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              花费(美元)
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {formatUsd(stats?.totalCostUsd ?? 0, 6)}
            </div>
            <div className="text-xs text-muted-foreground">
              今日 {formatUsd(stats?.todayCostUsd ?? 0, 6)}
            </div>
          </CardContent>
        </Card>
      </div>

      {/* 统一搜索 / 筛选 / 操作 toolbar */}
      <Card>
        <CardContent className="flex flex-wrap items-center gap-2 py-3">
          <div className="relative min-w-[260px] flex-1">
            <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="搜索模型 / 账号 / 会话 / 错误..."
              className="pl-8"
            />
          </div>

          <Select value={status} onValueChange={(v) => setStatus(v as never)}>
            <SelectTrigger className="w-32">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {STATUS_OPTIONS.map((o) => (
                <SelectItem key={o.value} value={o.value}>
                  {o.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          <Select value={source} onValueChange={(v) => setSource(v as never)}>
            <SelectTrigger className="w-32">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SOURCE_OPTIONS.map((o) => (
                <SelectItem key={o.value} value={o.value}>
                  {o.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          <Sheet open={filtersOpen} onOpenChange={setFiltersOpen}>
            <SheetTrigger asChild>
              <Button variant="outline" size="sm">
                <Filter className="h-4 w-4" />
                高级筛选
                {hasFilters && (
                  <Badge variant="secondary" className="ml-1">
                    生效中
                  </Badge>
                )}
              </Button>
            </SheetTrigger>
            <SheetContent>
              <SheetHeader>
                <SheetTitle>高级筛选</SheetTitle>
                <SheetDescription>
                  按模型、会话、账号、流式、最小缓存读 token 等维度精细筛选
                </SheetDescription>
              </SheetHeader>
              <div className="mt-4 space-y-3">
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground">模型</label>
                  <Input
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                    placeholder="claude-opus-4-7"
                  />
                </div>
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground">会话 ID</label>
                  <Input
                    value={conversationId}
                    onChange={(e) => setConversationId(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground">账号 ID</label>
                  <Input
                    value={credentialId}
                    onChange={(e) => setCredentialId(e.target.value)}
                    inputMode="numeric"
                  />
                </div>
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground">流式</label>
                  <Select
                    value={streamMode}
                    onValueChange={(v) => setStreamMode(v as never)}
                  >
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="all">全部</SelectItem>
                      <SelectItem value="stream">流式</SelectItem>
                      <SelectItem value="non_stream">非流式</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground">
                    最小 cache read tokens
                  </label>
                  <Input
                    value={minCacheRead}
                    onChange={(e) => setMinCacheRead(e.target.value)}
                    inputMode="numeric"
                    placeholder="例如 10000"
                  />
                </div>
              </div>
              <SheetFooter className="mt-6">
                <Button variant="outline" onClick={resetFilters} disabled={!hasFilters}>
                  <X className="h-4 w-4" />
                  重置
                </Button>
                <Button onClick={() => setFiltersOpen(false)}>应用</Button>
              </SheetFooter>
            </SheetContent>
          </Sheet>

          {hasFilters && (
            <Button variant="ghost" size="sm" onClick={resetFilters}>
              <X className="h-4 w-4" />
              重置筛选
            </Button>
          )}

          <div className="ml-auto flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                recordsQuery.refetch()
                statsQuery.refetch()
                summaryQuery.refetch()
              }}
            >
              <RefreshCw className="h-4 w-4" />
              刷新
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="text-destructive"
              onClick={handleClear}
              disabled={clearMutation.isPending}
            >
              <Trash2 className="h-4 w-4" />
              清空
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">请求记录</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          {recordsQuery.isLoading ? (
            <div className="py-8 text-center text-sm text-muted-foreground">加载中...</div>
          ) : recordsQuery.error ? (
            <div className="py-8 text-center text-sm text-destructive">
              {extractErrorMessage(recordsQuery.error)}
            </div>
          ) : (recordsQuery.data?.total ?? 0) === 0 ? (
            <div className="py-8 text-center text-sm text-muted-foreground">
              暂无记录
            </div>
          ) : (
            // 横向滚动 + 左侧 2 列(时间 / 账号)sticky 固定
            <div className="relative w-full overflow-x-auto rounded-md border">
              <table className="w-full caption-bottom text-sm">
                <thead className="bg-muted/40">
                  <tr className="border-b text-left text-xs text-muted-foreground">
                    <th className="sticky left-0 z-20 min-w-[140px] bg-muted/40 px-3 py-2 font-medium shadow-[1px_0_0_hsl(var(--border))]">
                      时间
                    </th>
                    <th className="sticky left-[140px] z-20 min-w-[160px] bg-muted/40 px-3 py-2 font-medium shadow-[1px_0_0_hsl(var(--border))]">
                      账号
                    </th>
                    <th className="min-w-[200px] px-3 py-2 font-medium">模型</th>
                    <th className="min-w-[180px] px-3 py-2 font-medium">会话</th>
                    <th className="min-w-[120px] px-3 py-2 font-medium">来源</th>
                    <th className="min-w-[100px] px-3 py-2 font-medium">状态</th>
                    <th className="min-w-[88px] px-3 py-2 text-right font-medium">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <span className="cursor-help underline decoration-dotted">Total In</span>
                        </TooltipTrigger>
                        <TooltipContent>原始输入 token(含缓存)</TooltipContent>
                      </Tooltip>
                    </th>
                    <th className="min-w-[88px] px-3 py-2 text-right font-medium">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <span className="cursor-help underline decoration-dotted">Compat In</span>
                        </TooltipTrigger>
                        <TooltipContent>兼容协议下的输入 token</TooltipContent>
                      </Tooltip>
                    </th>
                    <th className="min-w-[96px] px-3 py-2 text-right font-medium">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <span className="cursor-help underline decoration-dotted">Billable In</span>
                        </TooltipTrigger>
                        <TooltipContent>实际计费的输入 token(扣除缓存命中)</TooltipContent>
                      </Tooltip>
                    </th>
                    <th className="min-w-[96px] px-3 py-2 text-right font-medium">缓存读</th>
                    <th className="min-w-[96px] px-3 py-2 text-right font-medium">缓存写</th>
                    <th className="min-w-[80px] px-3 py-2 text-right font-medium">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <span className="cursor-help underline decoration-dotted">5m</span>
                        </TooltipTrigger>
                        <TooltipContent>5 分钟 ephemeral 缓存写入 token</TooltipContent>
                      </Tooltip>
                    </th>
                    <th className="min-w-[80px] px-3 py-2 text-right font-medium">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <span className="cursor-help underline decoration-dotted">1h</span>
                        </TooltipTrigger>
                        <TooltipContent>1 小时 persistent 缓存写入 token</TooltipContent>
                      </Tooltip>
                    </th>
                    <th className="min-w-[72px] px-3 py-2 text-right font-medium">Read %</th>
                    <th className="min-w-[80px] px-3 py-2 text-right font-medium">Cached %</th>
                    <th className="min-w-[80px] px-3 py-2 text-right font-medium">输出</th>
                    <th className="min-w-[110px] px-3 py-2 text-right font-medium">花费</th>
                    <th className="min-w-[80px] px-3 py-2 text-right font-medium">耗时</th>
                    <th className="min-w-[200px] px-3 py-2 font-medium">客户端</th>
                    <th className="min-w-[180px] px-3 py-2 font-medium">Request ID</th>
                  </tr>
                </thead>
                <tbody>
                  {recordsQuery.data?.records.map((r) => {
                    const primaryCredentialId = r.credentialId ?? r.lastAttemptedCredentialId
                    const credLabel =
                      typeof primaryCredentialId === 'number'
                        ? credentialLabels.get(primaryCredentialId) ?? r.credentialLabel
                        : r.credentialLabel
                    const attempts = r.attemptedCredentialIds || []
                    const rateLimited = uniqueIds(r.rateLimitedCredentialIds)
                    const traceTitle = credentialTraceTitle(r)
                    const cost = r.costUsd
                    const readRatio = r.totalInputTokens > 0
                      ? r.cacheReadInputTokens / r.totalInputTokens
                      : Number.NaN
                    const cachedRatio = r.totalInputTokens > 0
                      ? (r.cacheReadInputTokens + r.cacheCreationInputTokens) /
                        r.totalInputTokens
                      : Number.NaN
                    return (
                      <tr key={r.id} className="border-b transition-colors hover:bg-muted/30 last:border-0">
                        <td className="sticky left-0 z-10 whitespace-nowrap bg-background px-3 py-2 text-xs text-muted-foreground shadow-[1px_0_0_hsl(var(--border))]">
                          {formatDateTime(r.createdAt)}
                        </td>
                        <td className="sticky left-[140px] z-10 bg-background px-3 py-2 shadow-[1px_0_0_hsl(var(--border))]">
                          <div className="font-medium tabular-nums">
                            #{primaryCredentialId ?? '-'}
                          </div>
                          {credLabel && (
                            <div className="max-w-[140px] truncate text-xs text-muted-foreground" title={credLabel}>
                              {credLabel}
                            </div>
                          )}
                          {traceTitle && (
                            <div className="mt-1 max-w-[150px] truncate text-xs text-muted-foreground" title={traceTitle}>
                              {attempts.length > 0 && `尝试 ${attempts.map((id) => `#${id}`).join(' -> ')}`}
                              {attempts.length === 0 && rateLimited.length > 0 && `429 ${rateLimited.map((id) => `#${id}`).join(', ')}`}
                            </div>
                          )}
                        </td>
                        <td className="px-3 py-2">
                          <div className="max-w-[220px] truncate font-medium" title={r.model}>
                            {r.model || '-'}
                          </div>
                          <div className="mt-1 flex flex-wrap gap-1 text-xs">
                            <Badge variant="outline">{r.endpoint}</Badge>
                            <Badge variant={r.stream ? 'secondary' : 'outline'}>
                              {r.stream ? '流式' : '非流式'}
                            </Badge>
                            {r.simulated && <Badge variant="warning">模拟</Badge>}
                          </div>
                        </td>
                        <td className="px-3 py-2">
                          <div className="max-w-[200px] truncate font-mono text-xs" title={r.conversationId ?? ''}>
                            {r.conversationId ?? '-'}
                          </div>
                          <div className="mt-1 flex flex-wrap gap-1 text-xs">
                            {r.stickyBound && <Badge variant="secondary">sticky</Badge>}
                            {r.fallbackFromSticky && <Badge variant="warning">fallback</Badge>}
                          </div>
                        </td>
                        <td className="px-3 py-2">
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Badge variant={r.simulated ? 'warning' : 'secondary'}>
                                {usageSourceLabel(r.usageSource)}
                              </Badge>
                            </TooltipTrigger>
                            <TooltipContent>
                              {SOURCE_OPTIONS.find((s) => s.value === r.usageSource)?.tip ?? r.usageSource}
                            </TooltipContent>
                          </Tooltip>
                        </td>
                        <td className="px-3 py-2">
                          <Badge
                            variant={
                              r.status === 'success'
                                ? 'success'
                                : r.status === 'client_dropped'
                                  ? 'warning'
                                  : 'destructive'
                            }
                          >
                            {usageStatusLabel(r.status)}
                          </Badge>
                          {r.errorType && (
                            <div className="mt-1 max-w-[140px] truncate text-xs text-muted-foreground" title={r.errorType}>
                              {r.errorType}
                            </div>
                          )}
                          {r.errorMessage && (
                            <div className="mt-1 max-w-[160px] truncate text-xs text-destructive" title={r.errorMessage}>
                              {r.errorMessage}
                            </div>
                          )}
                        </td>
                        <td className="px-3 py-2 text-right tabular-nums">{formatNumber(r.totalInputTokens)}</td>
                        <td className="px-3 py-2 text-right tabular-nums">{formatNumber(r.compatInputTokens)}</td>
                        <td className="px-3 py-2 text-right tabular-nums">{formatNumber(r.billableInputTokens)}</td>
                        <td className="px-3 py-2 text-right tabular-nums">{formatNumber(r.cacheReadInputTokens)}</td>
                        <td className="px-3 py-2 text-right tabular-nums">{formatNumber(r.cacheCreationInputTokens)}</td>
                        <td className="px-3 py-2 text-right tabular-nums text-xs text-muted-foreground">
                          {formatNumber(r.cacheCreation5mInputTokens)}
                        </td>
                        <td className="px-3 py-2 text-right tabular-nums text-xs text-muted-foreground">
                          {formatNumber(r.cacheCreation1hInputTokens)}
                        </td>
                        <td className="px-3 py-2 text-right tabular-nums text-xs">
                          {formatPercent(readRatio, 1)}
                        </td>
                        <td className="px-3 py-2 text-right tabular-nums text-xs">
                          {formatPercent(cachedRatio, 1)}
                        </td>
                        <td className="px-3 py-2 text-right tabular-nums">{formatNumber(r.outputTokens)}</td>
                        <td className="px-3 py-2 text-right tabular-nums">
                          {cost == null ? (
                            <span className="text-muted-foreground">—</span>
                          ) : (
                            formatUsd(cost, 6)
                          )}
                        </td>
                        <td className="px-3 py-2 text-right tabular-nums">{r.durationMs}ms</td>
                        <td className="px-3 py-2">
                          <div className="max-w-[200px] truncate text-xs">
                            {r.clientUserAgent ? (
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <span className="truncate">{r.clientUserAgent}</span>
                                </TooltipTrigger>
                                <TooltipContent>{r.clientUserAgent}</TooltipContent>
                              </Tooltip>
                            ) : (
                              <span className="text-muted-foreground">—</span>
                            )}
                          </div>
                          <div className="text-xs text-muted-foreground tabular-nums">
                            {r.clientIp ?? ''}
                          </div>
                        </td>
                        <td className="px-3 py-2">
                          <code className="text-xs text-muted-foreground" title={r.requestId ?? r.id}>
                            {(r.requestId ?? r.id).slice(0, 18)}…
                          </code>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}

          {(recordsQuery.data?.total ?? 0) > 0 && (
            <Pagination
              page={page}
              totalPages={totalPages}
              totalItems={recordsQuery.data?.total ?? 0}
              pageSize={pageSize}
              pageSizeOptions={PAGE_SIZE_OPTIONS}
              onPageChange={setPage}
              onPageSizeChange={(size) => setPageSize(size)}
            />
          )}
        </CardContent>
      </Card>
    </div>
  )
}
