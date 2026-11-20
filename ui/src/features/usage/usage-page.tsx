import { useMemo, useState, type ReactNode } from 'react'
import { useRuntimeConfig } from '@/hooks/use-runtime-config'
import { useSearchParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import {
  Activity,
  BarChart3,
  Check,
  ChevronDown,
  ChevronUp,
  Clock3,
  Database,
  Download,
  DollarSign,
  SlidersHorizontal,
  Info,
  RefreshCw,
  Search,
  Trash2,
  X,
  Zap,
} from 'lucide-react'
import { toast } from 'sonner'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import {
  useUsageDashboardSeries,
  useUsageSummary,
  useUsageRecordsPage,
  useUsageCleanupStatus,
  useRefreshUsageQueriesAfterCleanup,
  useUsageWriterStats,
} from '@/hooks/use-usage'
import { useModelPricing, useSyncModelPricing } from '@/hooks/use-models'
import { getUsageRecords } from '@/api/usage-dashboard'
import { getAccounts } from '@/api/accounts'
import { getRoutes } from '@/api/routes'
import { displayRoutePrefix, sortRoutes } from '@/features/routes/route-utils'
import { cacheWriteTtlMarker, cacheWriteTtlTitle } from './cache-ttl-label'
import { formatDate, formatCompact, formatNumber, formatPercent, formatUsdCsv, formatUsdFixed2, ratio } from '@/lib/format'
import { normalizeRequestApiKeyId } from '@/lib/request-api-key-id'
import { cn, extractErrorMessage } from '@/lib/utils'
import type { Account, UsageRecord, UsageRecordStatus, UsageRecordsPageQuery, UsageRecordsQuery, UsageSource, UsageSeriesPoint } from '@/types/api'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  EmptyState,
  LoadingState,
  ErrorState,
  Callout,
  PAGE_SIZE_OPTIONS,
  Pagination,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Input,
  Popover,
  PopoverContent,
  PopoverTrigger,
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
import { TrendAreaChart, TrendBarChart, CHART_COLORS } from '@/components/charts'
import {
  statusLabel,
  statusTone,
  formatLatency,
  sourceLabel,
  formatExternalAttemptChain,
  upstreamModelLabel,
  upstreamResponseModelMismatch,
} from './usage-helpers'
import { UsageDetailModal } from './usage-detail-modal'
import { UsageCleanupModal } from './usage-cleanup-modal'
import { UsageCostInline, usageRecordCostModel } from './usage-billing'
import { RequestApiKeyIdDisplay } from './request-api-key-id'

// ─── 常量 ─────────────────────────────────────────────────────────────────────

const AUTO_REFRESH_KEY = 'claude-pool-admin:auto-refresh:usage'
const DEFAULT_PAGE_SIZE = 20
const EXPORT_LIMIT = 10_000
const ACCOUNT_OPTION_LIMIT = 50
const REQUEST_ID_PATTERN = /req_[A-Za-z0-9_-]+/
const SLOW_FIRST_TOKEN_MS = 10_000

type AccountSelectionValue = 'all' | `account:${number}`

// ─── 工具函数 ──────────────────────────────────────────────────────────────────

function seriesPointToRow(p: UsageSeriesPoint): Record<string, number | string> {
  return {
    label: p.label,
    requests: p.requests,
    errors: p.errorRequests,
    cost: p.totalEstimatedCostUsd,
    originalCost: p.totalOriginalCostUsd,
    inputTokens: p.totalInputTokens,
    outputTokens: p.totalOutputTokens,
  }
}

function usageInputTotal(record: UsageRecord): number {
  return record.compatInputTokens + record.cacheReadInputTokens + record.cacheCreationInputTokens
}

function routeAccountLabel(record: UsageRecord): string {
  const name = record.accountName ? ` ${record.accountName}` : ''
  return `账号 #${record.accountId ?? '-'}${name}`
}

function parseAccountSelection(value: AccountSelectionValue): number | undefined {
  if (value === 'all') return undefined
  const id = Number(value.slice('account:'.length))
  return Number.isFinite(id) && id > 0 ? id : undefined
}

function accountOptionLabel(account: Account): string {
  return `账号 #${account.id} ${account.name || '未命名'}`
}

function accountOptionMeta(account: Account): string {
  return [account.enabled ? '启用' : '停用', account.autoDisabled ? '自动禁用' : undefined, account.baseUrl].filter(Boolean).join(' · ')
}

function extractRequestId(value: string): string {
  return value.match(REQUEST_ID_PATTERN)?.[0] ?? ''
}

function toDatetimeLocalValue(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, '0')
  return [
    date.getFullYear(),
    '-',
    pad(date.getMonth() + 1),
    '-',
    pad(date.getDate()),
    ' ',
    pad(date.getHours()),
    ':',
    pad(date.getMinutes()),
  ].join('')
}

function isoToDatetimeLocalValue(value: string | null): string {
  if (!value) return ''
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) return ''
  return toDatetimeLocalValue(parsed)
}

function datetimeLocalToIso(value: string): string | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const normalized = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}/.test(trimmed)
    ? trimmed.replace(' ', 'T')
    : trimmed
  const date = new Date(normalized)
  if (Number.isNaN(date.getTime())) return undefined
  return date.toISOString()
}

function recentDatetimeLocal(hours: number): string {
  return toDatetimeLocalValue(new Date(Date.now() - hours * 60 * 60 * 1000))
}

function csvCell(value: unknown): string {
  if (value === null || typeof value === 'undefined') return ''
  const text = String(value)
  if (!/[",\n\r]/.test(text)) return text
  return `"${text.replace(/"/g, '""')}"`
}

function usageRecordsToCsv(records: UsageRecord[]): string {
  const headers = [
    'created_at',
    'request_id',
    'request_api_key_id',
    'conversation_id',
    'status',
    'stream',
    'endpoint',
    'requested_model',
    'upstream_model',
    'upstream_response_model',
    'account_id',
    'account_name',
    'usage_source',
    'total_input_tokens',
    'compat_input_tokens',
    'billable_input_tokens',
    'output_tokens',
    'cache_read_input_tokens',
    'cache_creation_input_tokens',
    'estimated_cost_usd',
    'original_cost_usd',
    'account_cost_usd',
    'pricing_model',
    'duration_ms',
    'first_token_latency_ms',
    'error_type',
    'error_message',
  ]
  const rows = records.map((record) => [
    record.createdAt,
    record.id,
    normalizeRequestApiKeyId(record.requestApiKeyId),
    record.conversationId,
    record.status,
    record.stream ? 'stream' : 'non_stream',
    record.endpoint,
    record.model,
    upstreamModelLabel(record) === '-' ? '' : upstreamModelLabel(record),
    record.upstreamResponseModel ?? '',
    record.accountId,
    record.accountName,
    record.usageSource,
    record.totalInputTokens,
    record.compatInputTokens,
    record.billableInputTokens,
    record.outputTokens,
    record.cacheReadInputTokens,
    record.cacheCreationInputTokens,
    formatUsdCsv(record.estimatedCostUsd),
    formatUsdCsv(record.originalCostUsd),
    formatUsdCsv(record.accountCostUsd),
    record.pricingModel,
    record.durationMs,
    record.firstTokenLatencyMs,
    record.publicErrorType || record.errorType,
    record.publicErrorMessage || record.errorMessage,
  ])
  return [headers, ...rows].map((row) => row.map(csvCell).join(',')).join('\n')
}

function downloadTextFile(content: string, filename: string, type: string) {
  const blob = new Blob([content], { type })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}

function FilterField({
  label,
  children,
  className,
}: {
  label: string
  children: ReactNode
  className?: string
}) {
  return (
    <label className={className}>
      <span className="mb-1 block text-[0.68rem] font-medium text-muted-foreground">{label}</span>
      {children}
    </label>
  )
}

const ALL_ROUTES = '__all__'

/** 路由筛选：值是路由前缀（默认路由为 "/"），与总览路由表跳过来的 `route` 参数一致。 */
function RouteTargetSelect({
  value,
  onChange,
}: {
  value: string
  onChange: (value: string) => void
}) {
  const routes = useQuery({ queryKey: ['routes'], queryFn: getRoutes, staleTime: 30_000 })
  const options = useMemo(
    () => sortRoutes(routes.data ?? []).map((route) => {
      const key = displayRoutePrefix(route.prefix)
      const name = route.name.trim()
      return { key, label: name && name !== key ? `${name} · ${key}` : key, enabled: route.enabled }
    }),
    [routes.data],
  )
  // 从总览带过来的前缀可能已在路由页删除：保留为一个选项，否则下拉显示不出当前条件。
  // 只有路由配置读取成功后才判断“已删除”；加载中或读取失败时只显示前缀本身。
  const missing = value !== ALL_ROUTES && !options.some((option) => option.key === value)
  const missingLabel = routes.isSuccess ? `${value}（已删除）` : value
  return (
    <Select value={value} onValueChange={onChange}>
      <SelectTrigger size="sm" className="w-full">
        <SelectValue placeholder="全部路由" />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={ALL_ROUTES}>全部路由</SelectItem>
        {options.map((option) => (
          <SelectItem key={option.key} value={option.key}>
            {option.label}{option.enabled ? '' : '（停用）'}
          </SelectItem>
        ))}
        {missing && <SelectItem value={value}>{missingLabel}</SelectItem>}
      </SelectContent>
    </Select>
  )
}

function AccountTargetSelect({
  value,
  onChange,
}: {
  value: AccountSelectionValue
  onChange: (value: AccountSelectionValue) => void
}) {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const debouncedSearch = useDebouncedValue(search, 250)
  const searchText = debouncedSearch.trim().toLowerCase()

  const accounts = useQuery({
    queryKey: ['accounts'],
    queryFn: getAccounts,
    enabled: open,
    staleTime: 30_000,
  })

  const filtered = useMemo(() => {
    const items = accounts.data?.accounts ?? []
    if (!searchText) return items.slice(0, ACCOUNT_OPTION_LIMIT)
    return items
      .filter((account) => {
        const haystack = [String(account.id), account.name, account.baseUrl, account.maskedApiKey, account.supportedModels?.join(' '), account.tags?.join(' ')]
          .filter(Boolean).join(' ').toLowerCase()
        return haystack.includes(searchText)
      })
      .slice(0, ACCOUNT_OPTION_LIMIT)
  }, [accounts.data?.accounts, searchText])

  const selectedLabel = useMemo(() => {
    const id = parseAccountSelection(value)
    if (id === undefined) return '全部账号'
    const account = accounts.data?.accounts.find((item) => item.id === id)
    return account ? accountOptionLabel(account) : `账号 #${id}`
  }, [accounts.data?.accounts, value])

  const selectValue = (next: AccountSelectionValue) => {
    onChange(next)
    setOpen(false)
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button variant="outline" size="sm" className="h-8 w-full justify-between overflow-hidden px-2 text-left">
          <span className="min-w-0 truncate">{selectedLabel}</span>
          <ChevronDown className="h-3.5 w-3.5 shrink-0 opacity-60" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-[min(28rem,calc(100vw-2rem))] p-2" align="start">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="搜索账号名称、地址、标签或模型"
            className="h-8 pl-7 text-xs"
          />
        </div>
        <div className="mt-2 max-h-72 overflow-y-auto pr-1 scrollbar-thin">
          <button
            type="button"
            className={cn(
              'flex w-full items-center justify-between rounded-md px-2 py-2 text-left text-xs hover:bg-muted',
              value === 'all' && 'bg-muted text-primary'
            )}
            onClick={() => selectValue('all')}
          >
            <span>全部账号</span>
            {value === 'all' && <Check className="h-3.5 w-3.5" />}
          </button>
          {accounts.isFetching && !accounts.data && (
            <div className="flex items-center gap-2 px-2 py-2 text-xs text-muted-foreground">
              <Spinner size="sm" />加载账号...
            </div>
          )}
          {accounts.data && filtered.length === 0 && (
            <div className="px-2 py-2 text-xs text-muted-foreground">没有匹配账号</div>
          )}
          {filtered.map((account) => {
            const itemValue = `account:${account.id}` as AccountSelectionValue
            return (
              <button
                key={itemValue}
                type="button"
                className={cn(
                  'flex w-full items-start justify-between gap-2 rounded-md px-2 py-2 text-left hover:bg-muted',
                  value === itemValue && 'bg-muted text-primary'
                )}
                onClick={() => selectValue(itemValue)}
              >
                <span className="min-w-0">
                  <span className="block truncate text-xs font-medium">{accountOptionLabel(account)}</span>
                  <span className="block truncate text-[0.65rem] text-muted-foreground">{accountOptionMeta(account)}</span>
                </span>
                {value === itemValue && <Check className="mt-0.5 h-3.5 w-3.5 shrink-0" />}
              </button>
            )
          })}
        </div>
      </PopoverContent>
    </Popover>
  )
}

// ─── 趋势图区 ─────────────────────────────────────────────────────────────────

function TrendView({ refetchInterval }: { refetchInterval: number | false }) {
  const dashboardSeries = useUsageDashboardSeries('Asia/Shanghai', refetchInterval)
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
          description="估算费用与原始计费趋势"
          actions={
            dailyData.length > 0
              ? <Badge tone="neutral">{formatUsdFixed2(dailyData.reduce((s, r) => s + Number(r.originalCost), 0))}</Badge>
              : undefined
          }
        >
          {dailyData.length === 0
            ? <EmptyState title="暂无数据" className="py-8" />
            : <TrendBarChart
                data={dailyData}
                xKey="label"
                series={[
                  { key: 'cost', name: '估算费用', color: CHART_COLORS[2] },
                  { key: 'originalCost', name: '原始计费', color: CHART_COLORS[4] },
                ]}
                height={200}
                valueFormatter={(v, key) =>
                  key === 'cost' || key === 'originalCost' ? formatUsdFixed2(Number(v)) : formatNumber(Number(v))
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
  const [searchParams] = useSearchParams()
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_SIZE)
  // 其它页面（总览排行、风控样本、账号卡）带过来的筛选条件全部从 URL 读，进页面即生效。
  const [q, setQ] = useState('')
  const [requestId, setRequestId] = useState(() => searchParams.get('requestId') ?? '')
  const [requestApiKeyId, setRequestApiKeyId] = useState(() => searchParams.get('requestApiKeyId') ?? '')
  const [model, setModel] = useState(() => searchParams.get('model') ?? '')
  const [endpoint, setEndpoint] = useState(() => searchParams.get('endpoint') ?? '')
  const [conversationId, setConversationId] = useState(() => searchParams.get('conversationId') ?? '')
  // 总览账号表、账号卡跳过来时带 accountId；老链接的 externalPoolId 也认。
  const initialAccountId = Number(searchParams.get('accountId') ?? searchParams.get('externalPoolId'))
  const initialAccountSelection: AccountSelectionValue =
    Number.isFinite(initialAccountId) && initialAccountId > 0 ? `account:${initialAccountId}` : 'all'
  const [accountSelection, setAccountSelection] = useState<AccountSelectionValue>(initialAccountSelection)
  // 总览路由表跳过来时带 route（路由前缀，默认路由为 "/"）。
  const [route, setRoute] = useState(() => searchParams.get('route')?.trim() || ALL_ROUTES)
  const initialStatus = searchParams.get('status')
  const [status, setStatus] = useState<UsageRecordStatus | '__all__'>(
    initialStatus && ['success', 'error', 'stream_error', 'upstream_timeout', 'client_dropped'].includes(initialStatus)
      ? (initialStatus as UsageRecordStatus)
      : '__all__',
  )
  const [source, setSource] = useState<UsageSource | '__all__'>('__all__')
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = useState('')
  const [minFirstTokenLatencyMs, setMinFirstTokenLatencyMs] = useState('')
  const [since, setSince] = useState(() => isoToDatetimeLocalValue(searchParams.get('since')))
  const [until, setUntil] = useState(() => isoToDatetimeLocalValue(searchParams.get('until')))
  // 从别的页面带着账号跳进来时直接展开高级筛选，让人看到生效的条件。
  const [advancedOpen, setAdvancedOpen] = useState(
    initialAccountSelection !== 'all' || Boolean(searchParams.get('requestApiKeyId') || searchParams.get('conversationId') || searchParams.get('status')),
  )
  const [exporting, setExporting] = useState(false)

  // 文本筛选项防抖,避免每次按键都触发查询(展示值即时,查询用防抖值)
  const qD = useDebouncedValue(q)
  const requestIdD = useDebouncedValue(requestId)
  const requestApiKeyIdD = useDebouncedValue(requestApiKeyId)
  const modelD = useDebouncedValue(model)
  const endpointD = useDebouncedValue(endpoint)
  const conversationIdD = useDebouncedValue(conversationId)
  const minCacheReadD = useDebouncedValue(minCacheRead)
  const minFirstTokenLatencyMsD = useDebouncedValue(minFirstTokenLatencyMs)
  const sinceD = useDebouncedValue(since)
  const untilD = useDebouncedValue(until)
  const hasAdvancedFilters =
    !!q.trim() || status !== '__all__' || source !== '__all__' || streamMode !== 'all' ||
    !!endpoint.trim() || !!conversationId.trim() || !!requestApiKeyId.trim() ||
    accountSelection !== 'all' || !!minCacheRead.trim()
  const showAdvancedFilters = advancedOpen || hasAdvancedFilters
  const selectedAccountId = useMemo(() => parseAccountSelection(accountSelection), [accountSelection])
  const normalizedRequestApiKeyId = normalizeRequestApiKeyId(requestApiKeyIdD)
  const requestApiKeyIdInvalid = Boolean(requestApiKeyId.trim() && !normalizeRequestApiKeyId(requestApiKeyId))

  const query = useMemo<UsageRecordsPageQuery>(() => {
    const next: UsageRecordsPageQuery = { page, limit: pageSize }
    const qValue = qD.trim()
    const requestIdInput = requestIdD.trim()
    const requestIdValue = extractRequestId(requestIdInput) || requestIdInput || extractRequestId(qValue)
    if (normalizedRequestApiKeyId) next.requestApiKeyId = normalizedRequestApiKeyId
    if (requestIdValue) {
      next.requestId = requestIdValue
      return next
    }
    if (qValue) next.q = qValue
    if (modelD.trim()) next.model = modelD.trim()
    if (endpointD.trim()) next.endpoint = endpointD.trim()
    if (conversationIdD.trim()) next.conversationId = conversationIdD.trim()
    if (selectedAccountId !== undefined) next.accountId = selectedAccountId
    if (route !== ALL_ROUTES) next.route = route
    if (status !== '__all__') next.status = status
    if (source !== '__all__') next.source = source
    if (streamMode !== 'all') next.stream = streamMode === 'stream'
    if (minCacheReadD.trim() && Number.isFinite(Number(minCacheReadD))) next.minCacheRead = Number(minCacheReadD)
    if (minFirstTokenLatencyMsD.trim() && Number.isFinite(Number(minFirstTokenLatencyMsD))) {
      next.minFirstTokenLatencyMs = Number(minFirstTokenLatencyMsD)
    }
    const sinceIso = datetimeLocalToIso(sinceD)
    const untilIso = datetimeLocalToIso(untilD)
    if (sinceIso) next.since = sinceIso
    if (untilIso) next.until = untilIso
    return next
  }, [
    conversationIdD,
    endpointD,
    minCacheReadD,
    minFirstTokenLatencyMsD,
    modelD,
    page,
    pageSize,
    qD,
    requestIdD,
    route,
    normalizedRequestApiKeyId,
    selectedAccountId,
    sinceD,
    source,
    status,
    streamMode,
    untilD,
  ])

  const records = useUsageRecordsPage(query, autoRefreshInterval)
  const items = records.data?.records ?? []
  const hasNext = records.data?.hasNext ?? false
  const pageTransitionPending = Boolean(
    records.data?.page !== undefined &&
    (records.isPlaceholderData || (records.isFetching && records.data.page !== page))
  )
  const hasFilters =
    status !== '__all__' || source !== '__all__' || streamMode !== 'all' ||
    !!q.trim() || !!requestId.trim() || !!requestApiKeyId.trim() || !!model.trim() ||
    !!endpoint.trim() || !!conversationId.trim() ||
    accountSelection !== 'all' || route !== ALL_ROUTES || !!minCacheRead.trim() ||
    !!minFirstTokenLatencyMs.trim() || !!since.trim() || !!until.trim()

  const clearFilters = () => {
    setQ(''); setRequestId(''); setRequestApiKeyId(''); setModel(''); setEndpoint(''); setConversationId('')
    setAccountSelection('all'); setRoute(ALL_ROUTES); setStatus('__all__'); setSource('__all__')
    setStreamMode('all'); setMinCacheRead(''); setMinFirstTokenLatencyMs(''); setSince(''); setUntil('')
  }

  const updateAccountSelection = (value: AccountSelectionValue) => {
    setAccountSelection(value)
    setPage(1)
  }

  const applyRecentHours = (hours: number) => {
    setSince(recentDatetimeLocal(hours))
    setUntil('')
    setPage(1)
  }

  const applySlowFirstTokenPreset = () => {
    setMinFirstTokenLatencyMs(String(SLOW_FIRST_TOKEN_MS))
    if (!since.trim() && !until.trim()) setSince(recentDatetimeLocal(6))
    setPage(1)
  }

  const exportCurrentQuery = async () => {
    setExporting(true)
    try {
      const { page: _page, ...queryWithoutPage } = query
      const exportQuery: UsageRecordsQuery = { ...queryWithoutPage, limit: EXPORT_LIMIT }
      const result = await getUsageRecords(exportQuery)
      if (result.records.length === 0) {
        toast.warning('当前筛选条件下没有可导出的用量记录')
        return
      }
      const csv = usageRecordsToCsv(result.records)
      const stamp = new Date().toISOString().replace(/[:.]/g, '-')
      downloadTextFile(csv, `claude-pool-usage-records-${stamp}.csv`, 'text/csv;charset=utf-8')
      const suffix = result.total > result.records.length
        ? `（最多导出 ${result.records.length}/${result.total} 条）`
        : ''
      toast.success(`已导出 ${result.records.length} 条用量记录${suffix}`)
    } catch (error) {
      toast.error(`导出失败: ${extractErrorMessage(error)}`)
    } finally {
      setExporting(false)
    }
  }

  return (
    <div className="space-y-3">
      <SectionCard
        title="明细记录"
        description="每次请求的完整记录，点击行查看详情"
        actions={
          <Button variant="outline" size="sm" onClick={exportCurrentQuery} disabled={exporting || records.isLoading}>
            {exporting ? <RefreshCw className="h-3.5 w-3.5 animate-spin" /> : <Download className="h-3.5 w-3.5" />}
            导出 CSV
          </Button>
        }
        noPadding
      >
        <div className="space-y-3 px-4 pt-4 pb-2">
          <div className="rounded-xl bg-card p-3 shadow-sm">
            {/* 8 个格子：中等宽度（含 1280）排成 4 列两行，保证起止时间框放得下完整的 `YYYY-MM-DD HH:mm`；
                足够宽时才排成一行，时间列设最小宽度。 */}
            <div className="grid gap-3 md:grid-cols-2 lg:grid-cols-4 2xl:grid-cols-[1.2fr_1fr_minmax(10rem,1fr)_minmax(10rem,1fr)_0.9fr_0.95fr_0.95fr_auto]">
              <FilterField label="请求 ID">
                <Input
                  placeholder="req_..."
                  value={requestId}
                  onChange={(e) => { setRequestId(e.target.value); setPage(1) }}
                  className="h-9 font-mono text-xs"
                />
              </FilterField>
              <FilterField label="模型">
                <Input
                  placeholder="claude-opus-4.8"
                  value={model}
                  onChange={(e) => { setModel(e.target.value); setPage(1) }}
                  className="h-9 text-xs"
                />
              </FilterField>
              <FilterField label="起始时间">
                <Input
                  placeholder="YYYY-MM-DD HH:mm"
                  value={since}
                  onChange={(e) => { setSince(e.target.value); setPage(1) }}
                  className="h-9 font-mono text-xs"
                />
              </FilterField>
              <FilterField label="结束时间">
                <Input
                  placeholder="YYYY-MM-DD HH:mm"
                  value={until}
                  onChange={(e) => { setUntil(e.target.value); setPage(1) }}
                  className="h-9 font-mono text-xs"
                />
              </FilterField>
              <FilterField label="首字不低于 ms">
                <Input
                  placeholder="10000"
                  value={minFirstTokenLatencyMs}
                  onChange={(e) => { setMinFirstTokenLatencyMs(e.target.value); setPage(1) }}
                  className="h-9 text-xs"
                  inputMode="numeric"
                />
              </FilterField>
              <FilterField label="账号">
                <AccountTargetSelect value={accountSelection} onChange={updateAccountSelection} />
              </FilterField>
              <FilterField label="路由">
                <RouteTargetSelect value={route} onChange={(value) => { setRoute(value); setPage(1) }} />
              </FilterField>
              <div className="flex items-end gap-2">
                <Button
                  variant={showAdvancedFilters ? 'secondary' : 'outline'}
                  size="sm"
                  onClick={() => setAdvancedOpen((v) => !v)}
                  className="h-9"
                >
                  <SlidersHorizontal className="h-3.5 w-3.5" />高级
                </Button>
                {hasFilters && (
                  <Button variant="ghost" size="sm" onClick={() => { clearFilters(); setPage(1) }} className="h-9">
                    <X className="h-3.5 w-3.5" />重置
                  </Button>
                )}
                {records.isFetching && <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />}
              </div>
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <Button variant="ghost" size="xs" onClick={() => applyRecentHours(1)}>最近 1h</Button>
              <Button variant="ghost" size="xs" onClick={() => applyRecentHours(6)}>最近 6h</Button>
              <Button variant="ghost" size="xs" onClick={applySlowFirstTokenPreset}>慢首字 &gt;10s</Button>
              <Button variant="ghost" size="xs" onClick={() => { setStatus('error'); setPage(1); setAdvancedOpen(true) }}>错误</Button>
              <Button variant="ghost" size="xs" onClick={() => { setStatus('client_dropped'); setPage(1); setAdvancedOpen(true) }}>客户端断开</Button>
            </div>

            {showAdvancedFilters && (
              <div className="mt-3 border-t pt-3">
                <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
                  <FilterField label="模糊搜索">
                    <Input
                      placeholder="错误、账号、路径、费用等"
                      value={q}
                      onChange={(e) => { setQ(e.target.value); setPage(1) }}
                      className="h-8 text-xs"
                    />
                  </FilterField>
                  <FilterField label="入口路径">
                    <Input
                      placeholder="/cc/v1/messages"
                      value={endpoint}
                      onChange={(e) => { setEndpoint(e.target.value); setPage(1) }}
                      className="h-8 text-xs"
                    />
                  </FilterField>
                  <FilterField label="会话 ID">
                    <Input
                      placeholder="conversation id"
                      value={conversationId}
                      onChange={(e) => { setConversationId(e.target.value); setPage(1) }}
                      className="h-8 font-mono text-xs"
                    />
                  </FilterField>
                  <FilterField label="请求渠道 ID">
                    <div>
                      <Input
                        placeholder="完整 64 位 SHA-256 digest"
                        value={requestApiKeyId}
                        maxLength={64}
                        aria-invalid={requestApiKeyIdInvalid}
                        onChange={(e) => { setRequestApiKeyId(e.target.value); setPage(1) }}
                        className="h-8 font-mono text-xs"
                      />
                      {requestApiKeyIdInvalid && (
                        <div className="mt-1 text-[0.68rem] text-destructive">无效值不会发送；请复制完整渠道 ID</div>
                      )}
                    </div>
                  </FilterField>
                  <FilterField label="最小缓存读取 token">
                    <Input
                      placeholder="如 10000"
                      value={minCacheRead}
                      onChange={(e) => { setMinCacheRead(e.target.value); setPage(1) }}
                      className="h-8 text-xs"
                      inputMode="numeric"
                    />
                  </FilterField>
                  <FilterField label="状态">
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
                  </FilterField>
                  <FilterField label="来源">
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
                  </FilterField>
                  <FilterField label="请求类型">
                    <Select value={streamMode} onValueChange={(v) => { setStreamMode(v as 'all' | 'stream' | 'non_stream'); setPage(1) }}>
                      <SelectTrigger size="sm"><SelectValue placeholder="全部请求" /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="all">全部请求</SelectItem>
                        <SelectItem value="stream">Stream</SelectItem>
                        <SelectItem value="non_stream">非 Stream</SelectItem>
                      </SelectContent>
                    </Select>
                  </FilterField>
                </div>
              </div>
            )}
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
                    <TableHead>账号</TableHead>
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
                    const reportedInputTotal = usageInputTotal(record)
                    const rowReadRatio = ratio(record.cacheReadInputTokens, reportedInputTotal)
                    const rowCachedRatio = ratio(
                      record.cacheReadInputTokens + record.cacheCreationInputTokens,
                      reportedInputTotal,
                    )
                    const externalChain = formatExternalAttemptChain(record)
                    const attemptCount = record.externalAttempts?.length ?? 0
                    const attemptSummary = attemptCount === 0
                      ? ''
                      : attemptCount === 1
                        ? '尝试 1 次'
                        : `尝试 ${attemptCount} 次 · 切换 ${new Set(record.externalAttempts?.map((attempt) => attempt.poolId)).size} 个账号`
                    const targetLabel = routeAccountLabel(record)
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
                          <div className="mt-1 flex max-w-[180px] items-center gap-1 text-[0.62rem] text-muted-foreground/70">
                            <span className="shrink-0">渠道</span>
                            <RequestApiKeyIdDisplay value={record.requestApiKeyId} />
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
                          {upstreamResponseModelMismatch(record) && (
                            <div
                              className="max-w-[220px] truncate font-mono text-[0.62rem] text-warning"
                              title={`上游实际返回 ${record.upstreamResponseModel}，返回给调用方的仍是请求模型`}
                            >
                              上游返回 {record.upstreamResponseModel}
                            </div>
                          )}
                          <div className="mt-1 flex flex-wrap gap-1">
                            <Badge title={record.endpoint || '-'}>{record.endpoint || '-'}</Badge>
                            {record.simulated && <Badge tone="warning">{sourceLabel(record.usageSource)}</Badge>}
                            {!record.simulated && <Badge tone="neutral">{sourceLabel(record.usageSource)}</Badge>}
                          </div>
                        </TableCell>
                        {/* 账号 */}
                        <TableCell>
                          <div className="max-w-[190px] truncate text-xs font-semibold" title={targetLabel}>
                            {targetLabel}
                          </div>
                          {record.accountId != null && (
                            <div className="max-w-[190px] truncate text-[0.68rem] text-muted-foreground/70" title={`账号成本 ${formatUsdFixed2(record.accountCostUsd)}`}>
                              账号成本 {formatUsdFixed2(record.accountCostUsd)}
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
                          <div className="text-primary" title={cacheWriteTtlTitle(record) ?? formatNumber(record.cacheCreationInputTokens)}>
                            写 {formatCompact(record.cacheCreationInputTokens)}
                            {cacheWriteTtlMarker(record) && <span className="ml-1 font-semibold">（{cacheWriteTtlMarker(record)}）</span>}
                          </div>
                          <div className="text-muted-foreground/60">{formatPercent(rowReadRatio)} / {formatPercent(rowCachedRatio)}</div>
                        </TableCell>
                        {/* 费用 */}
                        <TableCell className="text-right font-mono text-xs tabular-nums" onClick={(e) => e.stopPropagation()}>
                          <UsageCostInline model={usageRecordCostModel(record)} onViewDetail={() => onViewDetail(record)} />
                        </TableCell>
                        {/* 耗时 */}
                        <TableCell className="text-right font-mono text-xs tabular-nums">
                          <div>{formatLatency(record.durationMs)}</div>
                          <div className="text-muted-foreground/60">首字 {formatLatency(record.firstTokenLatencyMs)}</div>
                        </TableCell>
                        {/* 调用链路 */}
                        <TableCell>
                          {attemptSummary ? (
                            <div
                              className="max-w-[220px] truncate text-xs font-medium text-primary"
                              title={externalChain ? `${attemptSummary} · ${externalChain}` : attemptSummary}
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
            {(page > 1 || hasNext || pageSize !== DEFAULT_PAGE_SIZE) && (
              <div className="px-4 py-3">
                <Pagination
                  page={page}
                  hasNext={hasNext}
                  pageSize={pageSize}
                  pageSizeOptions={PAGE_SIZE_OPTIONS}
                  pending={pageTransitionPending}
                  onPageChange={setPage}
                  onPageSizeChange={(next) => { setPageSize(next); setPage(1) }}
                />
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
  const [trendOpen, setTrendOpen] = useState(false)
  const [selectedRecord, setSelectedRecord] = useState<UsageRecord | null>(null)
  const [cleanupOpen, setCleanupOpen] = useState(false)

  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)
  const summary = useUsageSummary(autoRefresh.refetchInterval)
  const cleanupStatus = useUsageCleanupStatus()
  const modelPricing = useModelPricing(autoRefresh.refetchInterval)
  const writerStats = useUsageWriterStats(autoRefresh.refetchInterval)
  const syncPricing = useSyncModelPricing()
  const runtimeConfig = useRuntimeConfig()
  const highCacheThreshold = runtimeConfig.data?.highCacheThreshold
  useRefreshUsageQueriesAfterCleanup(cleanupStatus.data)

  const data = summary.data
  const totalTokens = (data?.totalInputTokens ?? 0) + (data?.totalOutputTokens ?? 0)
  const errorRate = data && data.totalRequests > 0 ? data.errorRequests / data.totalRequests : 0
  const realtime = data?.realtime
  const realtimeWindow = realtime?.windowSeconds ?? 60
  // 缓存读 / (缓存读 + 缓存写 + 未缓存输入)，与总览口径一致；UsageSummary 的 totalInputTokens 已含缓存 token。
  const cacheRead = data?.totalCacheReadInputTokens ?? 0
  const cacheWrite = data?.totalCacheCreationInputTokens ?? 0
  const readRatio = ratio(cacheRead, data?.totalInputTokens ?? 0)
  const cachedRatio = ratio(cacheRead + cacheWrite, data?.totalInputTokens ?? 0)
  const billing = data?.accountBilling
  const pricingStatus = modelPricing.data
  // Records the writer gave up on (queue full or PgSQL timing out). They are missing from the
  // numbers on this page, so say how many instead of letting the page look complete.
  const droppedRecords = (writerStats.data?.droppedPersistRecords ?? 0) + (writerStats.data?.droppedRedisRecords ?? 0)

  const handleSyncPricing = () => {
    syncPricing.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) {
          toast.warning(`价格同步失败，继续使用${status.source === 'built-in' ? '内置价格' : '当前价格'}: ${status.lastError}`)
          return
        }
        toast.success(`价格已同步：${formatNumber(status.modelCount)} 个模型`)
        summary.refetch()
      },
      onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
    })
  }

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
        {syncPricing.isPending ? <Spinner size="sm" /> : <DollarSign className="h-3.5 w-3.5" />}
        同步价格
      </Button>
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
      {summary.error && <Callout tone="error">用量汇总加载失败：{extractErrorMessage(summary.error)}</Callout>}

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
              近 {realtimeWindow} 秒 · 成功 {formatNumber(realtime?.successRequests ?? 0)} · 错误 {formatNumber(realtime?.errorRequests ?? 0)} · TPM {formatCompact(realtime?.totalTpm ?? 0)}
            </span>
          }
          icon={<Zap />}
          tone="info"
        />
        <StatCard
          title="缓存命中较高"
          value={formatCompact(data?.highCacheRequests ?? 0)}
          valueTitle={formatNumber(data?.highCacheRequests ?? 0)}
          desc={typeof highCacheThreshold === 'number'
            ? `单次缓存读取 ≥ ${formatNumber(highCacheThreshold)} token 的请求（阈值在路由页"统计展示"里设）`
            : '单次缓存读取达到阈值的请求（阈值在路由页"统计展示"里设）'}
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
          value={formatCompact(cacheRead)}
          valueTitle={formatNumber(cacheRead)}
          desc={`命中率 ${Number.isFinite(readRatio) ? formatPercent(readRatio) : '-'} / 读+写占输入 ${Number.isFinite(cachedRatio) ? formatPercent(cachedRatio) : '-'}`}
          icon={<Zap />}
          tone="success"
        />
        <StatCard
          title="估算费用"
          value={formatUsdFixed2(data?.totalEstimatedCostUsd ?? 0)}
          desc={`计价覆盖 ${formatPercent(ratio(data?.pricedRequests ?? 0, data?.totalRequests ?? 1))}`}
          icon={<Clock3 />}
          tone="primary"
        />
        <StatCard
          title="原始计费"
          value={formatUsdFixed2(data?.totalOriginalCostUsd ?? 0)}
          desc="按上游原始 usage 估算"
          icon={<Clock3 />}
          tone="warning"
        />
        <StatCard
          title="上游原始成本"
          value={formatUsdFixed2(billing?.rawCostUsd ?? 0)}
          desc={`展示计费 ${formatUsdFixed2(billing?.shapedCostUsd ?? 0)} · 原始/整形倍率 ${billing && billing.rawCostUsd > 0 && (billing.shapedCostUsd ?? 0) > 0 ? (billing.rawCostUsd / (billing.shapedCostUsd ?? 1)).toFixed(3) : '-'}`}
          icon={<DollarSign />}
          tone="warning"
        />
        <StatCard
          title="可计费"
          value={formatUsdFixed2(billing?.billableCostUsd ?? 0)}
          desc={(
            <span>
              补偿后计费 {formatUsdFixed2(billing?.upliftedCostUsd ?? 0)} · 毛利{' '}
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
            <TrendView refetchInterval={autoRefresh.refetchInterval} />
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
        onClose={() => { setCleanupOpen(false); void summary.refetch() }}
      />
    </PageContainer>
  )
}
