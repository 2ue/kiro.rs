import { useMemo, useState, type ReactNode } from 'react'
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
  useModelPricing,
  useSyncModelPricing,
} from '@/hooks/use-usage'
import { getUsageRecords } from '@/api/usage'
import { getCredentialList, getExternalPools } from '@/api/credentials'
import { formatDate, formatCompact, formatNumber, formatPercent, formatUsdCsv, formatUsdFixed2, ratio } from '@/lib/format'
import { normalizeRequestApiKeyId } from '@/lib/request-api-key-id'
import { cn, extractErrorMessage } from '@/lib/utils'
import type { CredentialListItem, ExternalPool, UsageRecord, UsageRecordStatus, UsageRecordsPageQuery, UsageRecordsQuery, UsageRouteKindFilter, UsageSource, UsageSeriesPoint } from '@/types/api'
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
import { UsageCostInline, usageRecordCostModel } from './usage-billing'
import { RequestApiKeyIdDisplay } from './request-api-key-id'

// ─── 常量 ─────────────────────────────────────────────────────────────────────

const AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:usage'
const PAGE_SIZE = 20
const EXPORT_LIMIT = 10_000
const ROUTE_OPTION_LIMIT = 50
const REQUEST_ID_PATTERN = /req_[A-Za-z0-9_-]+/
const SLOW_FIRST_TOKEN_MS = 10_000

type RouteSelectionValue = 'all' | `credential:${number}` | `external:${number}`

type ParsedRouteSelection =
  | { kind: 'all' }
  | { kind: 'credential'; id: number }
  | { kind: 'external'; id: number }

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

function routeAccountLabel(record: UsageRecord, credentialLabel?: string): string {
  if (record.routeKind === 'external_pool') {
    const name = record.externalPoolName ? ` ${record.externalPoolName}` : ''
    return `外部账号 #${record.externalPoolId ?? '-'}${name}`
  }
  const label = credentialLabel ? ` ${credentialLabel}` : ''
  return `账号 #${record.credentialId ?? '-'}${label}`
}

function parseRouteSelection(value: RouteSelectionValue): ParsedRouteSelection {
  if (value === 'all') return { kind: 'all' }
  const [kind, rawId] = value.split(':')
  const id = Number(rawId)
  if (!Number.isFinite(id) || id <= 0) return { kind: 'all' }
  if (kind === 'credential') return { kind: 'credential', id }
  if (kind === 'external') return { kind: 'external', id }
  return { kind: 'all' }
}

function credentialOptionLabel(credential: CredentialListItem): string {
  const identity = credential.email || credential.maskedApiKey || credential.refreshTokenHash || credential.apiKeyHash || '未命名账号'
  return `账号 #${credential.id} ${identity}`
}

function credentialOptionMeta(credential: CredentialListItem): string {
  const parts = [
    credential.subscriptionTitle,
    credential.provider,
    credential.effectiveApiRegion ? `api ${credential.effectiveApiRegion}` : undefined,
    credential.disabled ? '已禁用' : undefined,
  ].filter(Boolean)
  return parts.join(' · ')
}

function externalPoolOptionLabel(pool: ExternalPool): string {
  return `外部池 #${pool.id} ${pool.name || '未命名'}`
}

function externalPoolOptionMeta(pool: ExternalPool): string {
  return [pool.enabled ? '启用' : '禁用', pool.baseUrl].filter(Boolean).join(' · ')
}

function routeSelectionAllLabel(routeKind: UsageRouteKindFilter | '__all__'): string {
  if (routeKind === 'local_credential') return '全部本地账号'
  if (routeKind === 'external_pool') return '全部外部池'
  return '全部账号/外部池'
}

function routeSelectionFallbackLabel(value: RouteSelectionValue, routeKind: UsageRouteKindFilter | '__all__'): string {
  const parsed = parseRouteSelection(value)
  if (parsed.kind === 'credential') return `账号 #${parsed.id}`
  if (parsed.kind === 'external') return `外部池 #${parsed.id}`
  return routeSelectionAllLabel(routeKind)
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
    'route_kind',
    'route_subtype',
    'credential_id',
    'credential_label',
    'external_pool_id',
    'external_pool_name',
    'usage_source',
    'total_input_tokens',
    'compat_input_tokens',
    'billable_input_tokens',
    'output_tokens',
    'cache_read_input_tokens',
    'cache_creation_input_tokens',
    'estimated_cost_usd',
    'original_cost_usd',
    'kiro_metering_usage',
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
    record.routeKind,
    record.routeSubtype,
    record.credentialId,
    record.credentialLabel,
    record.externalPoolId,
    record.externalPoolName,
    record.usageSource,
    record.totalInputTokens,
    record.compatInputTokens,
    record.billableInputTokens,
    record.outputTokens,
    record.cacheReadInputTokens,
    record.cacheCreationInputTokens,
    formatUsdCsv(record.estimatedCostUsd),
    formatUsdCsv(record.originalCostUsd),
    record.kiroMeteringUsage,
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

function RouteTargetSelect({
  value,
  routeKind,
  onChange,
}: {
  value: RouteSelectionValue
  routeKind: UsageRouteKindFilter | '__all__'
  onChange: (value: RouteSelectionValue) => void
}) {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const debouncedSearch = useDebouncedValue(search, 250)
  const searchText = debouncedSearch.trim()
  const showCredentials = routeKind !== 'external_pool'
  const showExternalPools = routeKind !== 'local_credential'

  const credentials = useQuery({
    queryKey: ['usage-route-target-credentials', searchText],
    queryFn: () => getCredentialList({
      page: 1,
      limit: ROUTE_OPTION_LIMIT,
      q: searchText || undefined,
    }),
    enabled: open && showCredentials,
    staleTime: 30_000,
  })

  const externalPools = useQuery({
    queryKey: ['usage-route-target-external-pools'],
    queryFn: getExternalPools,
    enabled: open && showExternalPools,
    staleTime: 30_000,
  })

  const filteredPools = useMemo(() => {
    const pools = externalPools.data?.pools ?? []
    const q = searchText.toLowerCase()
    if (!q) return pools.slice(0, ROUTE_OPTION_LIMIT)
    return pools
      .filter((pool) => {
        const haystack = [
          String(pool.id),
          pool.name,
          pool.baseUrl,
          pool.maskedApiKey,
          pool.supportedModels?.join(' '),
        ].filter(Boolean).join(' ').toLowerCase()
        return haystack.includes(q)
      })
      .slice(0, ROUTE_OPTION_LIMIT)
  }, [externalPools.data?.pools, searchText])

  const selectedLabel = useMemo(() => {
    const parsed = parseRouteSelection(value)
    if (parsed.kind === 'credential') {
      const credential = credentials.data?.items.find((item) => item.id === parsed.id)
      return credential ? credentialOptionLabel(credential) : routeSelectionFallbackLabel(value, routeKind)
    }
    if (parsed.kind === 'external') {
      const pool = externalPools.data?.pools.find((item) => item.id === parsed.id)
      return pool ? externalPoolOptionLabel(pool) : routeSelectionFallbackLabel(value, routeKind)
    }
    return routeSelectionAllLabel(routeKind)
  }, [credentials.data?.items, externalPools.data?.pools, routeKind, value])

  const selectValue = (next: RouteSelectionValue) => {
    onChange(next)
    setOpen(false)
  }

  const credentialsLoading = showCredentials && credentials.isFetching
  const poolsLoading = showExternalPools && externalPools.isFetching
  const hasCredentialItems = (credentials.data?.items.length ?? 0) > 0
  const hasPoolItems = filteredPools.length > 0

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
            placeholder="搜索账号邮箱、key、外部池名称"
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
            <span>{routeSelectionAllLabel(routeKind)}</span>
            {value === 'all' && <Check className="h-3.5 w-3.5" />}
          </button>

          {showCredentials && (
            <div className="mt-1">
              <div className="px-2 py-1 text-[0.65rem] font-medium text-muted-foreground">本地账号</div>
              {credentialsLoading && (
                <div className="flex items-center gap-2 px-2 py-2 text-xs text-muted-foreground">
                  <Spinner size="sm" />加载账号...
                </div>
              )}
              {!credentialsLoading && !hasCredentialItems && (
                <div className="px-2 py-2 text-xs text-muted-foreground">没有匹配账号</div>
              )}
              {(credentials.data?.items ?? []).map((credential) => {
                const itemValue = `credential:${credential.id}` as RouteSelectionValue
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
                      <span className="block truncate text-xs font-medium">{credentialOptionLabel(credential)}</span>
                      <span className="block truncate text-[0.65rem] text-muted-foreground">
                        {credentialOptionMeta(credential) || credential.endpoint}
                      </span>
                    </span>
                    {value === itemValue && <Check className="mt-0.5 h-3.5 w-3.5 shrink-0" />}
                  </button>
                )
              })}
            </div>
          )}

          {showExternalPools && (
            <div className="mt-1">
              <div className="px-2 py-1 text-[0.65rem] font-medium text-muted-foreground">外部池</div>
              {poolsLoading && (
                <div className="flex items-center gap-2 px-2 py-2 text-xs text-muted-foreground">
                  <Spinner size="sm" />加载外部池...
                </div>
              )}
              {!poolsLoading && !hasPoolItems && (
                <div className="px-2 py-2 text-xs text-muted-foreground">没有匹配外部池</div>
              )}
              {filteredPools.map((pool) => {
                const itemValue = `external:${pool.id}` as RouteSelectionValue
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
                      <span className="block truncate text-xs font-medium">{externalPoolOptionLabel(pool)}</span>
                      <span className="block truncate text-[0.65rem] text-muted-foreground">
                        {externalPoolOptionMeta(pool)}
                      </span>
                    </span>
                    {value === itemValue && <Check className="mt-0.5 h-3.5 w-3.5 shrink-0" />}
                  </button>
                )
              })}
            </div>
          )}
        </div>
      </PopoverContent>
    </Popover>
  )
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
  const [page, setPage] = useState(1)
  const [q, setQ] = useState('')
  const [requestId, setRequestId] = useState('')
  const [requestApiKeyId, setRequestApiKeyId] = useState('')
  const [model, setModel] = useState('')
  const [endpoint, setEndpoint] = useState('')
  const [conversationId, setConversationId] = useState('')
  const [routeSelection, setRouteSelection] = useState<RouteSelectionValue>('all')
  const [routeKind, setRouteKind] = useState<UsageRouteKindFilter | '__all__'>('__all__')
  const [status, setStatus] = useState<UsageRecordStatus | '__all__'>('__all__')
  const [source, setSource] = useState<UsageSource | '__all__'>('__all__')
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = useState('')
  const [minFirstTokenLatencyMs, setMinFirstTokenLatencyMs] = useState('')
  const [since, setSince] = useState('')
  const [until, setUntil] = useState('')
  const [advancedOpen, setAdvancedOpen] = useState(false)
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
    routeSelection !== 'all' || !!minCacheRead.trim()
  const showAdvancedFilters = advancedOpen || hasAdvancedFilters
  const selectedRouteTarget = useMemo(() => parseRouteSelection(routeSelection), [routeSelection])
  const normalizedRequestApiKeyId = normalizeRequestApiKeyId(requestApiKeyIdD)
  const requestApiKeyIdInvalid = Boolean(requestApiKeyId.trim() && !normalizeRequestApiKeyId(requestApiKeyId))

  const query = useMemo<UsageRecordsPageQuery>(() => {
    const next: UsageRecordsPageQuery = { page, limit: PAGE_SIZE }
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
    if (routeKind !== '__all__') next.routeKind = routeKind
    if (selectedRouteTarget.kind === 'credential') next.credentialId = selectedRouteTarget.id
    if (selectedRouteTarget.kind === 'external') next.externalPoolId = selectedRouteTarget.id
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
    qD,
    requestIdD,
    normalizedRequestApiKeyId,
    routeKind,
    selectedRouteTarget,
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
    routeKind !== '__all__' || status !== '__all__' || source !== '__all__' || streamMode !== 'all' ||
    !!q.trim() || !!requestId.trim() || !!requestApiKeyId.trim() || !!model.trim() ||
    !!endpoint.trim() || !!conversationId.trim() ||
    routeSelection !== 'all' || !!minCacheRead.trim() ||
    !!minFirstTokenLatencyMs.trim() || !!since.trim() || !!until.trim()

  const clearFilters = () => {
    setQ(''); setRequestId(''); setRequestApiKeyId(''); setModel(''); setEndpoint(''); setConversationId('')
    setRouteSelection('all'); setRouteKind('__all__'); setStatus('__all__'); setSource('__all__')
    setStreamMode('all'); setMinCacheRead(''); setMinFirstTokenLatencyMs(''); setSince(''); setUntil('')
  }

  const updateRouteKind = (value: UsageRouteKindFilter | '__all__') => {
    const selected = parseRouteSelection(routeSelection)
    if (value === 'local_credential' && selected.kind === 'external') setRouteSelection('all')
    if (value === 'external_pool' && selected.kind === 'credential') setRouteSelection('all')
    setRouteKind(value)
    setPage(1)
  }

  const updateRouteSelection = (value: RouteSelectionValue) => {
    setRouteSelection(value)
    const selected = parseRouteSelection(value)
    if (selected.kind === 'credential') setRouteKind('local_credential')
    if (selected.kind === 'external') setRouteKind('external_pool')
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
      downloadTextFile(csv, `kiro-usage-records-${stamp}.csv`, 'text/csv;charset=utf-8')
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
            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-[1.2fr_1fr_1fr_1fr_0.9fr_0.95fr_auto]">
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
              <FilterField label="路由">
                <Select value={routeKind} onValueChange={(v) => updateRouteKind(v as UsageRouteKindFilter | '__all__')}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__all__">全部路由</SelectItem>
                    <SelectItem value="local_credential">本地账号</SelectItem>
                    <SelectItem value="external_pool">外部池</SelectItem>
                  </SelectContent>
                </Select>
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
              <Button variant="ghost" size="xs" onClick={() => updateRouteKind('external_pool')}>外部池</Button>
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
                  <FilterField label="账号 / 外部池">
                    <RouteTargetSelect
                      value={routeSelection}
                      routeKind={routeKind}
                      onChange={updateRouteSelection}
                    />
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
                    const label = record.credentialLabel
                    const reportedInputTotal = usageInputTotal(record)
                    const rowReadRatio = ratio(record.cacheReadInputTokens, reportedInputTotal)
                    const rowCachedRatio = ratio(
                      record.cacheReadInputTokens + record.cacheCreationInputTokens,
                      reportedInputTotal,
                    )
                    const attemptSummary = formatAttemptSummary(record)
                    const attemptChain = formatAttemptChain(record)
                    const externalChain = formatExternalAttemptChain(record)
                    const targetLabel = routeAccountLabel(record, label)
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
                          <UsageCostInline model={usageRecordCostModel(record)} onViewDetail={() => onViewDetail(record)} />
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
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={page === 1 || pageTransitionPending}
                    onClick={() => setPage((v) => Math.max(1, v - 1))}
                  >
                    上一页
                  </Button>
                  <span className="text-xs text-muted-foreground">
                    第 {page} 页，每页 {PAGE_SIZE} 条{pageTransitionPending ? ' · 加载中' : ''}
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
  const syncPricing = useSyncModelPricing()
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
  const pricingStatus = modelPricing.data

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
