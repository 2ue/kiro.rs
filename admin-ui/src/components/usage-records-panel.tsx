import { useEffect, useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { DollarSign, Download, Info, RefreshCw, Trash2, X } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useCredentials } from '@/hooks/use-credentials'
import {
  useCancelUsageCleanup,
  useClearUsageRecords,
  useModelPricing,
  usePreviewUsageCleanup,
  useRefreshUsageQueriesAfterCleanup,
  useResumeUsageCleanup,
  useStartUsageCleanup,
  useSyncModelPricing,
  useUsageCleanupStatus,
  useUsageRecordsPage,
  useUsageSummary,
} from '@/hooks/use-usage'
import { getExternalPools } from '@/api/credentials'
import { getUsageRecords } from '@/api/usage'
import { extractErrorMessage } from '@/lib/utils'
import { formatUsd, formatUsdCsv, formatUsdDetailed } from '@/lib/format'
import { normalizeRequestApiKeyId } from '@/lib/request-api-key-id'
import type { ExternalPoolUsageSnapshot, UsageCleanupMode, UsageCleanupRequest, UsageRecord, UsageRecordsPageQuery, UsageRecordStatus, UsageSource } from '@/types/api'
import { RequestApiKeyIdDisplay } from '@/components/request-api-key-id'

const USAGE_AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:usage'
const REQUEST_ID_PATTERN = /^req_[A-Za-z0-9_-]+$/
const EXPORT_LIMIT = 10_000
const SLOW_FIRST_TOKEN_MS = 10_000

type BillingDeltaTone = 'loss' | 'profit' | 'even'

function billingDeltaTone(delta: number): BillingDeltaTone {
  if (delta < 0) return 'loss'
  if (delta > 0) return 'profit'
  return 'even'
}

function billingDeltaTextClass(tone: BillingDeltaTone): string {
  if (tone === 'loss') return 'text-kiro-error'
  if (tone === 'profit') return 'text-kiro-warning'
  return 'text-muted-foreground'
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat('zh-CN').format(value)
}

function formatMeteringUsage(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const num = value as number
  return new Intl.NumberFormat('zh-CN', {
    maximumFractionDigits: num >= 1 ? 3 : 6,
  }).format(num)
}

function formatLatency(value?: number): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return '-'
  if (value < 1000) return `${formatNumber(Math.round(value))}ms`
  return `${(value / 1000).toFixed(2)}s`
}

const UPSTREAM_EVENT_TYPE_LABELS: Record<string, string> = {
  assistant_response: 'assistant',
  tool_use: 'tool',
  reasoning_content: 'thinking',
  metadata: 'metadata',
  metering: 'metering',
  code: 'code',
  context_usage: 'context',
  message_metadata: 'message_meta',
  invalid_state: 'invalid',
  unknown: 'unknown',
  error: 'error',
  exception: 'exception',
}

function formatUpstreamEventTypeCounts(counts?: Record<string, number>): string {
  if (!counts) return '-'
  const entries = Object.entries(counts)
    .filter(([, count]) => typeof count === 'number' && Number.isFinite(count) && count > 0)
    .sort((a, b) => b[1] - a[1])
  if (entries.length === 0) return '-'
  return entries
    .map(([kind, count]) => `${UPSTREAM_EVENT_TYPE_LABELS[kind] || kind} ${formatNumber(count)}`)
    .join(' / ')
}

function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return `${(value * 100).toFixed(1)}%`
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

function datetimeLocalToIso(value: string): string | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const date = new Date(trimmed)
  if (Number.isNaN(date.getTime())) return undefined
  return date.toISOString()
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
    case 'queued':
      return '排队中'
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
    case 'local_rescue_after_external':
      return '备用池后回本地'
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
    `输入 ${formatNumber(snapshot.inputTokens)}`,
    `输出 ${formatNumber(snapshot.outputTokens)}`,
    `读 ${formatNumber(snapshot.cacheReadInputTokens)}`,
    `写 ${formatNumber(snapshot.cacheCreationInputTokens)}`,
  ].join(' / ')
}

function routeVariant(record: UsageRecord): 'success' | 'secondary' | 'outline' | 'warning' | 'destructive' {
  if (record.routeSubtype === 'external_direct_policy') return 'warning'
  if (record.routeSubtype === 'local_rescue_after_external') return 'secondary'
  if (record.routeKind === 'external_pool') return record.status === 'success' ? 'success' : 'destructive'
  return 'outline'
}

function upstreamModel(record: UsageRecord): string {
  if (record.routeKind === 'external_pool') {
    return record.externalOutboundModel || record.upstreamModel || record.model || '-'
  }
  return record.upstreamModel || record.model || '-'
}

function upstreamModelLabel(record: UsageRecord): string {
  if (record.routeKind === 'external_pool' && record.externalOutboundModel) {
    return upstreamModel(record)
  }
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

function formatAttemptSummary(record: UsageRecord): string {
  const attempts = record.credentialAttempts || []
  if (attempts.length === 0) return '无本地尝试'

  const uniqueCredentialIds = new Set(attempts.map((attempt) => attempt.credentialId))
  if (attempts.length === 1) {
    return record.fallbackFromSticky ? '本地尝试 1 次 · sticky换号' : '本地尝试 1 次'
  }
  if (uniqueCredentialIds.size <= 1) return `本地尝试 ${attempts.length} 次 · 同账号重试 ${attempts.length - 1} 次`
  return `本地尝试 ${attempts.length} 次 · 切换 ${uniqueCredentialIds.size} 个账号`
}

function formatExternalAttemptChain(record: UsageRecord): string {
  return (record.externalAttempts || [])
    .map((attempt) => {
      const model = attempt.outboundModel ? ` ${attempt.outboundModel}` : ''
      return `外部池 #${attempt.poolId}${model}(${attempt.status ?? attempt.errorType ?? attempt.action})`
    })
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

function UsageMetric({
  label,
  value,
  tone = 'default',
}: {
  label: string
  value: string
  tone?: 'default' | 'success' | 'info' | 'warning'
}) {
  const toneClass =
    tone === 'success'
      ? 'text-kiro-success'
      : tone === 'info'
        ? 'text-primary'
        : tone === 'warning'
          ? 'text-amber-600'
        : 'text-foreground'
  return (
    <div className="rounded-md border bg-card px-2.5 py-1.5">
      <div className="text-[0.68rem] font-medium text-muted-foreground">{label}</div>
      <div className={`mt-0.5 truncate font-mono text-[0.82rem] font-semibold ${toneClass}`}>
        {value}
      </div>
    </div>
  )
}

function LatencyTracePanel({ record }: { record: UsageRecord }) {
  const trace = record.latencyTrace
  if (!trace) return null

  const firstOutput = trace.firstOutputDeltaMs ?? record.firstTokenLatencyMs
  return (
    <div className="rounded-md border bg-background p-3 text-sm">
      <div className="mb-2 font-medium">耗时拆解</div>
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <UsageMetric label="总耗时" value={formatLatency(record.durationMs)} />
        <UsageMetric label="请求检查" value={formatLatency(trace.payloadGuardMs)} />
        <UsageMetric label="上游响应头" value={formatLatency(trace.upstreamHeaderMs)} tone="info" />
        <UsageMetric label="首个流分片" value={formatLatency(trace.firstUpstreamChunkMs)} />
        <UsageMetric label="首次输出" value={formatLatency(firstOutput)} tone="success" />
        <UsageMetric label="首次思考" value={formatLatency(trace.firstThinkingDeltaMs)} />
        <UsageMetric label="首次可见文本" value={formatLatency(trace.firstVisibleTextDeltaMs)} tone="success" />
        <UsageMetric label="分片到输出" value={formatLatency(trace.streamGapToFirstOutputMs)} />
        <UsageMetric label="推理发送" value={trace.inferenceAttempts ? `${formatNumber(trace.inferenceAttempts.consumed)} / ${formatNumber(trace.inferenceAttempts.maxAttempts)}` : '-'} tone={trace.inferenceAttempts?.exhausted ? 'warning' : 'info'} />
        <UsageMetric label="推理发送分项" value={trace.inferenceAttempts ? `本地 ${formatNumber(trace.inferenceAttempts.localAttempts)} / 外部 ${formatNumber(trace.inferenceAttempts.externalAttempts)} / MCP ${formatNumber(trace.inferenceAttempts.mcpAttempts)}` : '-'} />
        <UsageMetric label="辅助发送" value={trace.auxiliaryAttempts ? `${formatNumber(trace.auxiliaryAttempts.consumed)} / ${formatNumber(trace.auxiliaryAttempts.maxAttempts)}` : '-'} tone={trace.auxiliaryAttempts?.exhausted ? 'warning' : 'info'} />
        <UsageMetric label="辅助发送分项" value={trace.auxiliaryAttempts ? `刷新 ${formatNumber(trace.auxiliaryAttempts.tokenRefreshAttempts)} / Profile ${formatNumber(trace.auxiliaryAttempts.profileDiscoveryAttempts)}` : '-'} />
        <UsageMetric label="本地容量权重" value={typeof trace.capacityWeightUnits === 'number' ? `${formatNumber(trace.capacityWeightUnits)} 单位` : '-'} tone="info" />
        <UsageMetric label="权重估算输入" value={typeof trace.estimatedInputTokens === 'number' ? `${formatNumber(trace.estimatedInputTokens)} token` : '-'} />
        <UsageMetric label="输出前分片" value={typeof trace.chunksBeforeFirstOutput === 'number' ? formatNumber(trace.chunksBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前事件" value={typeof trace.eventsBeforeFirstOutput === 'number' ? formatNumber(trace.eventsBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前上游字节" value={typeof trace.upstreamBytesBeforeFirstOutput === 'number' ? formatNumber(trace.upstreamBytesBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前上游帧" value={typeof trace.upstreamFramesBeforeFirstOutput === 'number' ? formatNumber(trace.upstreamFramesBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前上游事件" value={typeof trace.upstreamEventsBeforeFirstOutput === 'number' ? formatNumber(trace.upstreamEventsBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前空转换帧" value={typeof trace.upstreamFramesWithoutDownstreamEventsBeforeFirstOutput === 'number' ? formatNumber(trace.upstreamFramesWithoutDownstreamEventsBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前待解码分片" value={typeof trace.upstreamPendingChunksBeforeFirstOutput === 'number' ? formatNumber(trace.upstreamPendingChunksBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前帧解码错" value={typeof trace.upstreamFrameDecodeErrorsBeforeFirstOutput === 'number' ? formatNumber(trace.upstreamFrameDecodeErrorsBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前事件解析错" value={typeof trace.upstreamEventParseErrorsBeforeFirstOutput === 'number' ? formatNumber(trace.upstreamEventParseErrorsBeforeFirstOutput) : '-'} />
        <UsageMetric label="输出前上游类型" value={formatUpstreamEventTypeCounts(trace.upstreamEventTypesBeforeFirstOutput)} />
        <UsageMetric label="首输出前重试" value={typeof trace.streamRetryAttempts === 'number' ? formatNumber(trace.streamRetryAttempts) : '-'} tone="warning" />
        <UsageMetric label="重试调度失败" value={typeof trace.streamRetryDispatchFailures === 'number' ? formatNumber(trace.streamRetryDispatchFailures) : '-'} tone="warning" />
        <UsageMetric label="重试原因" value={trace.streamRetryReasons?.length ? trace.streamRetryReasons.join(' / ') : '-'} />
        <UsageMetric label="客户端断开" value={formatLatency(trace.clientDroppedMs)} />
        <UsageMetric label="结束原因" value={trace.terminalReason || '-'} />
        <UsageMetric label="上游完成状态" value={trace.upstreamMessageStatus || '-'} tone={trace.sawUpstreamCompleted ? 'success' : 'info'} />
        <UsageMetric label="上游显式完成" value={typeof trace.sawUpstreamCompleted === 'boolean' ? (trace.sawUpstreamCompleted ? '是' : '否') : '-'} tone={trace.sawUpstreamCompleted ? 'success' : 'warning'} />
        <UsageMetric label="结束原因来源" value={trace.stopReasonSource || '-'} />
        <UsageMetric label="疑似开场白空转" value={trace.suspectedIntentPreambleEndTurn ? '是' : '-'} tone={trace.suspectedIntentPreambleEndTurn ? 'warning' : 'default'} />
        <UsageMetric label="开场白风险" value={trace.intentPreambleRisk || '-'} tone={trace.intentPreambleRisk === 'high' ? 'warning' : 'info'} />
        <UsageMetric label="EndTurn异常原因" value={trace.endTurnAnomalyReason || '-'} tone={trace.endTurnAnomalyRisk === 'high' ? 'warning' : 'info'} />
        <UsageMetric label="EndTurn异常风险" value={trace.endTurnAnomalyRisk || '-'} tone={trace.endTurnAnomalyRisk === 'high' ? 'warning' : 'info'} />
        <UsageMetric label="疑似工具上下文泄漏" value={trace.suspectedToolContextLeakEndTurn ? '是' : '-'} tone={trace.suspectedToolContextLeakEndTurn ? 'warning' : 'default'} />
        <UsageMetric label="工具泄漏标记" value={trace.toolContextLeakMarkers?.length ? trace.toolContextLeakMarkers.join(' / ') : '-'} tone={trace.toolContextLeakMarkers?.length ? 'warning' : 'default'} />
        <UsageMetric label="尾部意图提示" value={trace.assistantTailIntentHint ? '是' : '-'} tone={trace.assistantTailIntentHint ? 'info' : 'default'} />
        <UsageMetric label="EOF无显式完成" value={typeof trace.upstreamEofWithoutCompleted === 'boolean' ? (trace.upstreamEofWithoutCompleted ? '是' : '否') : '-'} tone={trace.upstreamEofWithoutCompleted ? 'warning' : 'success'} />
        <UsageMetric label="最后上游事件" value={trace.lastUpstreamEventType || '-'} />
        <UsageMetric label="上游事件尾部" value={trace.lastUpstreamEvents?.length ? trace.lastUpstreamEvents.join(' → ') : '-'} />
        <UsageMetric label="见到Assistant事件" value={typeof trace.sawUpstreamAssistantResponse === 'boolean' ? (trace.sawUpstreamAssistantResponse ? '是' : '否') : '-'} />
        <UsageMetric label="见到ToolUse事件" value={typeof trace.sawUpstreamToolUse === 'boolean' ? (trace.sawUpstreamToolUse ? '是' : '否') : '-'} />
        <UsageMetric label="见到Metadata事件" value={typeof trace.sawUpstreamMetadata === 'boolean' ? (trace.sawUpstreamMetadata ? '是' : '否') : '-'} />
        <UsageMetric label="最后Assistant字符" value={typeof trace.lastAssistantContentChars === 'number' ? formatNumber(trace.lastAssistantContentChars) : '-'} />
        <UsageMetric label="过滤trivial文本块" value={typeof trace.filteredTrivialTextBlocks === 'number' ? formatNumber(trace.filteredTrivialTextBlocks) : '-'} tone="warning" />
        <UsageMetric label="过滤trivial字符" value={typeof trace.filteredTrivialTextChars === 'number' ? formatNumber(trace.filteredTrivialTextChars) : '-'} tone="warning" />
      </div>
    </div>
  )
}

export function UsageRecordsPanel() {
  const [searchText, setSearchText] = useState('')
  const [requestApiKeyId, setRequestApiKeyId] = useState('')
  const [model, setModel] = useState('')
  const [endpoint, setEndpoint] = useState('')
  const [conversationId, setConversationId] = useState('')
  const [routeTarget, setRouteTarget] = useState('')
  const [status, setStatus] = useState<UsageRecordStatus | ''>('')
  const [source, setSource] = useState<UsageSource | ''>('')
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = useState('')
  const [minFirstTokenLatencyMs, setMinFirstTokenLatencyMs] = useState('')
  const [since, setSince] = useState('')
  const [until, setUntil] = useState('')
  const [selectedRecord, setSelectedRecord] = useState<UsageRecord | null>(null)
  const [cleanupOpen, setCleanupOpen] = useState(false)
  const [currentPage, setCurrentPage] = useState(1)
  const [exporting, setExporting] = useState(false)
  const itemsPerPage = 20
  const autoRefresh = useAutoRefreshPreference(USAGE_AUTO_REFRESH_KEY)
  const normalizedRequestApiKeyId = normalizeRequestApiKeyId(requestApiKeyId)
  const requestApiKeyIdInvalid = Boolean(requestApiKeyId.trim() && !normalizedRequestApiKeyId)

  const query = useMemo<UsageRecordsPageQuery>(() => {
    const next: UsageRecordsPageQuery = { page: currentPage, limit: itemsPerPage }
    const qValue = searchText.trim()
    if (normalizedRequestApiKeyId) next.requestApiKeyId = normalizedRequestApiKeyId
    if (qValue) {
      if (REQUEST_ID_PATTERN.test(qValue)) next.requestId = qValue
      else next.q = qValue
    }
    if (model.trim()) {
      next.model = model.trim()
    }
    if (endpoint.trim()) {
      next.endpoint = endpoint.trim()
    }
    if (conversationId.trim()) {
      next.conversationId = conversationId.trim()
    }
    const [routeType, routeId] = routeTarget.split(':')
    const parsedRouteId = Number(routeId)
    if (routeTarget && Number.isFinite(parsedRouteId)) {
      if (routeType === 'credential') next.credentialId = parsedRouteId
      if (routeType === 'external') next.externalPoolId = parsedRouteId
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
    const parsedMinFirstTokenLatency = Number(minFirstTokenLatencyMs)
    if (minFirstTokenLatencyMs.trim() && Number.isFinite(parsedMinFirstTokenLatency)) {
      next.minFirstTokenLatencyMs = Math.max(0, Math.floor(parsedMinFirstTokenLatency))
    }
    const sinceIso = datetimeLocalToIso(since)
    if (sinceIso) next.since = sinceIso
    const untilIso = datetimeLocalToIso(until)
    if (untilIso) next.until = untilIso
    return next
  }, [conversationId, currentPage, endpoint, minCacheRead, minFirstTokenLatencyMs, model, normalizedRequestApiKeyId, routeTarget, searchText, since, source, status, streamMode, until])

  const summary = useUsageSummary(autoRefresh.refetchInterval)
  const records = useUsageRecordsPage(query, autoRefresh.refetchInterval)
  const modelPricing = useModelPricing(autoRefresh.refetchInterval)
  const syncPricing = useSyncModelPricing()
  const credentials = useCredentials({ refetchInterval: autoRefresh.refetchInterval })
  const externalPools = useQuery({ queryKey: ['external-pools'], queryFn: getExternalPools, refetchInterval: autoRefresh.refetchInterval })

  useEffect(() => {
    setCurrentPage(1)
  }, [conversationId, minCacheRead, minFirstTokenLatencyMs, model, requestApiKeyId, routeTarget, searchText, since, source, status, streamMode, until])

  const credentialLabels = useMemo(() => {
    const labels = new Map<number, string>()
    for (const credential of credentials.data?.credentials || []) {
      labels.set(
        credential.id,
        credential.email || credential.maskedApiKey || `账号 #${credential.id}`
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

  const handleExportCsv = async () => {
    setExporting(true)
    try {
      const { page: _page, ...queryWithoutPage } = query
      const result = await getUsageRecords({ ...queryWithoutPage, limit: EXPORT_LIMIT })
      if (result.records.length === 0) {
        toast.warning('当前筛选条件下没有可导出的用量记录')
        return
      }
      downloadTextFile(
        usageRecordsToCsv(result.records),
        `kiro-usage-${new Date().toISOString().replace(/[:.]/g, '-')}.csv`,
        'text/csv;charset=utf-8'
      )
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

  const hasFilters = Boolean(
    searchText.trim() ||
    requestApiKeyId.trim() ||
    model.trim() ||
    endpoint.trim() ||
    conversationId.trim() ||
    routeTarget ||
    status ||
    source ||
    streamMode !== 'all' ||
    minCacheRead.trim() ||
    minFirstTokenLatencyMs.trim() ||
    since.trim() ||
    until.trim()
  )

  const handleResetFilters = () => {
    setSearchText('')
    setRequestApiKeyId('')
    setModel('')
    setEndpoint('')
    setConversationId('')
    setRouteTarget('')
    setStatus('')
    setSource('')
    setStreamMode('all')
    setMinCacheRead('')
    setMinFirstTokenLatencyMs('')
    setSince('')
    setUntil('')
  }

  const summaryData = summary.data
  const pageRecords = records.data?.records || []
  const hasNextPage = Boolean(records.data?.hasNext)
  const recordsPage = records.data?.page
  const pageTransitionPending = recordsPage !== undefined && (records.isPlaceholderData || (records.isFetching && recordsPage !== currentPage))
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
      <div className="grid gap-4 md:grid-cols-3 xl:grid-cols-7">
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
              近 {realtimeWindow} 秒 {formatNumber(realtime?.requests || 0)} 请求 · 成功 {formatNumber(realtime?.successRequests || 0)} · 错误 {formatNumber(realtime?.errorRequests || 0)}
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
              按上报输入 + 上报输出统计
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">高缓存请求</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold text-kiro-success">{formatNumber(summaryData?.highCacheRequests || 0)}</div>
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
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">原始计费</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{formatUsd(summaryData?.totalOriginalCostUsd || 0)}</div>
            <div className="text-xs text-muted-foreground">按上游原始 usage 估算</div>
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
            value={endpoint}
            onChange={(event) => setEndpoint(event.target.value)}
            placeholder="入口路径，如 /cc/v1/messages"
          />
          <Input
            value={conversationId}
            onChange={(event) => setConversationId(event.target.value)}
            placeholder="会话 ID"
          />
          <div>
            <Input
              value={requestApiKeyId}
              maxLength={64}
              aria-invalid={requestApiKeyIdInvalid}
              onChange={(event) => setRequestApiKeyId(event.target.value)}
              placeholder="请求渠道 ID（64 位 digest）"
              className="font-mono"
            />
            {requestApiKeyIdInvalid && (
              <div className="mt-1 text-xs text-destructive">无效值不会发送；请复制完整渠道 ID</div>
            )}
          </div>
          <select
            value={routeTarget}
            onChange={(event) => setRouteTarget(event.target.value)}
            className="h-10 rounded-md border border-input bg-background px-3 py-2 text-sm"
          >
            <option value="">全部账号/外部池</option>
            {(credentials.data?.credentials || []).length > 0 && (
              <optgroup label="账号凭证">
                {(credentials.data?.credentials || []).map((credential) => (
                  <option key={`credential:${credential.id}`} value={`credential:${credential.id}`}>
                    #{credential.id} {credential.email || credential.maskedApiKey || '未命名账号'}
                  </option>
                ))}
              </optgroup>
            )}
            {(externalPools.data?.pools || []).length > 0 && (
              <optgroup label="外部池">
                {(externalPools.data?.pools || []).map((pool) => (
                  <option key={`external:${pool.id}`} value={`external:${pool.id}`}>
                    #{pool.id} {pool.name}
                  </option>
                ))}
              </optgroup>
            )}
          </select>
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
          <Input
            value={minFirstTokenLatencyMs}
            onChange={(event) => setMinFirstTokenLatencyMs(event.target.value)}
            placeholder="首字延迟 ≥ ms"
            inputMode="numeric"
          />
          <Input
            value={since}
            onChange={(event) => setSince(event.target.value)}
            type="datetime-local"
            title="开始时间"
          />
          <Input
            value={until}
            onChange={(event) => setUntil(event.target.value)}
            type="datetime-local"
            title="结束时间"
          />
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex cursor-pointer items-center gap-2 text-xs text-muted-foreground">
            <input
              type="checkbox"
              className="h-4 w-4"
              checked={autoRefresh.enabled}
              onChange={(event) => autoRefresh.setEnabled(event.target.checked)}
            />
            自动刷新
          </label>
          <Input
            type="number"
            min={5}
            max={3600}
            className="h-8 w-20"
            value={autoRefresh.intervalSeconds}
            disabled={!autoRefresh.enabled}
            onChange={(event) => autoRefresh.setIntervalSeconds(Number(event.target.value))}
          />
          <span className="text-xs text-muted-foreground">秒</span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setMinFirstTokenLatencyMs(String(SLOW_FIRST_TOKEN_MS))}
          >
            慢首字 &gt; 10s
          </Button>
          <Button variant="outline" size="sm" onClick={handleResetFilters} disabled={!hasFilters}>
            <X className="h-4 w-4" />
            重置
          </Button>
          <Button variant="outline" size="sm" onClick={handleRefresh}>
            <RefreshCw className="h-4 w-4" />
            刷新
          </Button>
          <Button variant="outline" size="sm" onClick={handleExportCsv} disabled={exporting}>
            <Download className="h-4 w-4" />
            {exporting ? '导出中' : '导出 CSV'}
          </Button>
          <Button variant="outline" size="sm" onClick={() => setCleanupOpen(true)}>
            <Trash2 className="h-4 w-4" />
            分批清理
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
              <table className="w-full min-w-[1560px] text-sm">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">时间</th>
                    <th className="px-3 py-2 font-medium">账号</th>
                    <th className="px-3 py-2 font-medium">模型</th>
                    <th className="px-3 py-2 font-medium">会话</th>
                    <th className="px-3 py-2 font-medium">来源</th>
                    <th className="px-3 py-2 font-medium">状态</th>
                    <th className="px-3 py-2 font-medium text-right">上报输入</th>
                    <th className="px-3 py-2 font-medium text-right">上报缓存读取</th>
                    <th className="px-3 py-2 font-medium text-right">上报缓存写入</th>
                    <th className="px-3 py-2 font-medium text-right">读取率</th>
                    <th className="px-3 py-2 font-medium text-right">缓存率</th>
                    <th className="px-3 py-2 font-medium text-right">上报输出</th>
                    <th className="px-3 py-2 font-medium text-right">费用</th>
                    <th className="px-3 py-2 font-medium text-right">耗时 / 首字</th>
                  </tr>
                </thead>
                <tbody>
                  {pageRecords.map((record) => {
                    const credentialLabel =
                      typeof record.credentialId === 'number'
                        ? credentialLabels.get(record.credentialId) || record.credentialLabel
                        : record.credentialLabel
                    const reportedInputTotal =
                      record.compatInputTokens +
                      record.cacheReadInputTokens +
                      record.cacheCreationInputTokens
                    const readRatio = ratio(record.cacheReadInputTokens, reportedInputTotal)
                    const cachedRatio = ratio(
                      record.cacheReadInputTokens + record.cacheCreationInputTokens,
                      reportedInputTotal
                    )
                    const attemptChain = formatAttemptChain(record)
                    const attemptSummary = formatAttemptSummary(record)
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
                            title={`${attemptSummary} · ${attemptChain}`}
                          >
                            链路 {attemptSummary}
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
                        <div className="mt-1 flex max-w-[220px] items-center gap-1 text-xs text-muted-foreground">
                          <span className="shrink-0">渠道</span>
                          <RequestApiKeyIdDisplay value={record.requestApiKeyId} />
                        </div>
                        <div className="mt-1 flex flex-wrap gap-1">
                          {record.stickyBound && <Badge variant="secondary">sticky</Badge>}
                          {record.fallbackFromSticky && (
                            <Badge
                              variant="warning"
                              title="Sticky 绑定账号不可用或当时无法承接，调度回退到其他本地账号；调用链路展示实际上游尝试。"
                            >
                              sticky回退
                            </Badge>
                          )}
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
                      <td className="px-3 py-2 text-right">{formatNumber(record.compatInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.cacheReadInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.cacheCreationInputTokens)}</td>
                      <td className="px-3 py-2 text-right">{formatPercent(readRatio)}</td>
                      <td className="px-3 py-2 text-right">{formatPercent(cachedRatio)}</td>
                      <td className="px-3 py-2 text-right">{formatNumber(record.outputTokens)}</td>
                      <td className="px-3 py-2 text-right">
                        <button
                          type="button"
                          className="font-medium text-primary underline-offset-2 hover:underline"
                          onClick={() => setSelectedRecord(record)}
                          title="查看计费明细"
                        >
                          {formatUsdDetailed(record.estimatedCostUsd || 0)}
                        </button>
                        <div className="text-xs text-amber-600">
                          原始 {formatUsdDetailed(record.originalCostUsd || 0)}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          Kiro {formatMeteringUsage(record.kiroMeteringUsage || 0)}
                        </div>
                      </td>
                      <td className="px-3 py-2 text-right">
                        <div className="flex items-center justify-end gap-2">
                          <span>{formatLatency(record.durationMs)}</span>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="h-7 w-7"
                            onClick={() => setSelectedRecord(record)}
                            title="查看 usage 口径和详情"
                          >
                            <Info className="h-4 w-4" />
                          </Button>
                        </div>
                        <div className="text-xs text-muted-foreground">
                          首字 {formatLatency(record.firstTokenLatencyMs)}
                        </div>
                      </td>
                    </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
          {(currentPage > 1 || hasNextPage || pageTransitionPending) && (
            <div className="mt-4 flex items-center justify-center gap-4">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                disabled={currentPage === 1 || pageTransitionPending}
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
                disabled={!hasNextPage || pageTransitionPending}
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
                  <div className="text-xs text-muted-foreground">请求渠道 ID</div>
                  <RequestApiKeyIdDisplay value={selectedRecord.requestApiKeyId} />
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">时间</div>
                  <div>{formatDate(selectedRecord.createdAt)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">请求模型</div>
                  <div className="break-all">{selectedRecord.model || '-'}</div>
                </div>
                {selectedRecord.requestedMaxTokens != null && (
                  <div>
                    <div className="text-xs text-muted-foreground">请求 max_tokens</div>
                    <div>{formatNumber(selectedRecord.requestedMaxTokens)}</div>
                  </div>
                )}
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
                    {formatUsdDetailed(selectedRecord.estimatedCostUsd || 0)}
                    <span className="ml-2 text-xs text-muted-foreground">
                      {selectedRecord.pricingAvailable
                        ? selectedRecord.pricingModel || 'priced'
                        : 'unpriced'}
                    </span>
                  </div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">原始计费</div>
                  <div>{formatUsdDetailed(selectedRecord.originalCostUsd || 0)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">Kiro计量</div>
                  <div>{formatMeteringUsage(selectedRecord.kiroMeteringUsage || 0)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">首字 token</div>
                  <div>{formatLatency(selectedRecord.firstTokenLatencyMs)}</div>
                </div>
                {selectedRecord.publicErrorType && (
                  <div>
                    <div className="text-xs text-muted-foreground">客户端错误类型</div>
                    <div className="break-all">{selectedRecord.publicErrorType}</div>
                  </div>
                )}
                {selectedRecord.publicErrorStatusCode != null && (
                  <div>
                    <div className="text-xs text-muted-foreground">客户端状态码</div>
                    <div>{selectedRecord.publicErrorStatusCode}</div>
                  </div>
                )}
                {selectedRecord.publicErrorMessage && (
                  <div className="md:col-span-2">
                    <div className="text-xs text-muted-foreground">客户端收到的错误</div>
                    <div className="break-all">{selectedRecord.publicErrorMessage}</div>
                  </div>
                )}
                {selectedRecord.errorType && (
                  <div>
                    <div className="text-xs text-muted-foreground">内部错误类型</div>
                    <div className="break-all">{selectedRecord.errorType}</div>
                  </div>
                )}
                {selectedRecord.errorStatusCode != null && (
                  <div>
                    <div className="text-xs text-muted-foreground">内部状态码</div>
                    <div>{selectedRecord.errorStatusCode}</div>
                  </div>
                )}
                {selectedRecord.errorSource && (
                  <div>
                    <div className="text-xs text-muted-foreground">错误阶段</div>
                    <div className="break-all">{selectedRecord.errorSource}</div>
                  </div>
                )}
                {selectedRecord.errorId && (
                  <div>
                    <div className="text-xs text-muted-foreground">错误 ID</div>
                    <div className="break-all font-mono">{selectedRecord.errorId}</div>
                  </div>
                )}
              </div>
              <LatencyTracePanel record={selectedRecord} />
              <div className="rounded-md border bg-muted/30 p-3 text-sm">
                <div className="mb-2 flex flex-wrap items-center gap-2">
                  <div className="font-medium">Usage 口径</div>
                  <span className="text-xs text-muted-foreground">
                    本地估算输入仅用于诊断；下游响应用量以上报字段为准。
                  </span>
                </div>
                <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-6">
                  <UsageMetric label="本地估算输入" value={formatNumber(selectedRecord.totalInputTokens)} />
                  <UsageMetric label="上报输入" value={formatNumber(selectedRecord.compatInputTokens)} />
                  <UsageMetric label="上报缓存写入" value={formatNumber(selectedRecord.cacheCreationInputTokens)} tone="info" />
                  <UsageMetric label="上报缓存读取" value={formatNumber(selectedRecord.cacheReadInputTokens)} tone="success" />
                  <UsageMetric label="上报输出" value={formatNumber(selectedRecord.outputTokens)} />
                  <UsageMetric label="内部成本输入" value={formatNumber(selectedRecord.billableInputTokens)} />
                </div>
                <div className="mt-2 text-xs leading-5 text-muted-foreground">
                  内部成本输入 = 上报输入 + 上报缓存写入，仅用于本系统费用估算和历史兼容，不是 Anthropic/Kiro 响应里的独立字段。
                </div>
              </div>
              {selectedRecord.externalPoolBilling && (
                (() => {
                  const billing = selectedRecord.externalPoolBilling
                  const shapedCost = billing.shapedCostUsd ?? billing.reportedCostUsd ?? 0
                  const upliftedCost = billing.upliftedCostUsd ?? billing.reportedCostUsd ?? billing.billableCostUsd ?? 0
                  const profit = billing.profitUsd ?? (upliftedCost - (billing.rawCostUsd || 0))
                  const deltaTone = billingDeltaTone(profit)
                  const hasLoss = deltaTone === 'loss'
                  const hasProfit = deltaTone === 'profit'
                  return (
                    <div className="rounded-md border bg-muted/30 p-3 text-sm">
                      <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
                        <div className="font-medium">外部池计费拆分</div>
                        <Badge variant={hasLoss ? 'destructive' : hasProfit ? 'warning' : 'success'}>
                          {hasLoss ? `亏损 ${formatUsdDetailed(Math.abs(profit))}` : hasProfit ? `盈利 ${formatUsdDetailed(profit)}` : '持平'}
                        </Badge>
                      </div>
                      <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                        <div>
                          <div className="text-xs text-muted-foreground">上游原始 usage 成本</div>
                          <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.rawUsage)}</div>
                          <div className="mt-1 font-medium">{formatUsdDetailed(billing.rawCostUsd || 0)}</div>
                          <div className="text-xs text-muted-foreground">按外部上游返回 usage 估算</div>
                        </div>
                        <div>
                          <div className="text-xs text-muted-foreground">整形后计费</div>
                          <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.shapedUsage || billing.reportedUsage)}</div>
                          <div className="mt-1 font-medium">{formatUsdDetailed(shapedCost)}</div>
                          <div className="text-xs text-muted-foreground">按当前路径缓存策略整形，未放大</div>
                        </div>
                        <div>
                          <div className="text-xs text-muted-foreground">整形后放大计费</div>
                          <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.reportedUsage)}</div>
                          <div className="mt-1 font-medium">{formatUsdDetailed(upliftedCost)}</div>
                          <div className={`text-xs ${billingDeltaTextClass(deltaTone)}`}>
                            盈利 = 放大后 - 上游原始：{profit >= 0 ? '+' : ''}{formatUsdDetailed(profit)}
                          </div>
                        </div>
                        <div>
                          <div className="text-xs text-muted-foreground">计价模型 / 整形模式</div>
                          <div className="break-all">{billing.pricingAvailable ? billing.pricingModel || 'priced' : 'unpriced'}</div>
                          <div className="text-xs text-muted-foreground">
                            {billing.usageProjectionMode} · {billing.usageProjectionApplied ? '已按当前路径整形' : '未整形/透传'}
                          </div>
                        </div>
                      </div>
                    </div>
                  )
                })()
              )}
              {(selectedRecord.credentialAttempts || []).length > 0 && (
                <div>
                  <div className="mb-2 flex flex-wrap items-center gap-2 text-sm">
                    <span className="font-medium">调用链路</span>
                    <Badge variant="secondary">{formatAttemptSummary(selectedRecord)}</Badge>
                  </div>
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
                            <td className="px-3 py-2 text-right">{formatLatency(attempt.durationMs)}</td>
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
                            <td className="px-3 py-2 text-right">{formatLatency(attempt.durationMs)}</td>
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
              {selectedRecord.errorMetadata != null && (
                <div>
                  <div className="mb-2 text-sm font-medium">错误元数据</div>
                  <pre className="max-h-[360px] overflow-auto rounded-md border bg-muted p-3 text-xs whitespace-pre-wrap break-words">
                    {formatJsonBlock(selectedRecord.errorMetadata)}
                  </pre>
                </div>
              )}
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
    case 'paused':
      return '已暂停'
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
const USAGE_CLEANUP_MAX_OLDER_THAN_DAYS = 3650
const USAGE_CLEANUP_DEFAULT_BATCH_SIZE = 250
const USAGE_CLEANUP_MAX_BATCH_SIZE = 500
const USAGE_CLEANUP_MAX_PAUSE_MS = 10000

function parseCleanupInteger(value: string, fallback: number, min: number, max: number): number {
  const parsed = Number(value)
  const normalized = Number.isFinite(parsed) ? Math.floor(parsed) : fallback
  return Math.max(min, Math.min(max, normalized))
}

function UsageCleanupDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const [mode, setMode] = useState<UsageCleanupMode>('soft_delete')
  const [olderThanDays, setOlderThanDays] = useState('7')
  const [batchSize, setBatchSize] = useState(String(USAGE_CLEANUP_DEFAULT_BATCH_SIZE))
  const [pauseMs, setPauseMs] = useState('100')
  const cleanupStatus = useUsageCleanupStatus()
  const previewCleanup = usePreviewUsageCleanup()
  const startCleanup = useStartUsageCleanup()
  const cancelCleanup = useCancelUsageCleanup()
  const clearRecords = useClearUsageRecords()
  const resumeCleanup = useResumeUsageCleanup()
  useRefreshUsageQueriesAfterCleanup(cleanupStatus.data)

  const parsedOlderThanDays = parseCleanupInteger(olderThanDays, 7, 0, USAGE_CLEANUP_MAX_OLDER_THAN_DAYS)
  const parsedBatchSize = parseCleanupInteger(batchSize, USAGE_CLEANUP_DEFAULT_BATCH_SIZE, 1, USAGE_CLEANUP_MAX_BATCH_SIZE)
  const parsedPauseMs = parseCleanupInteger(pauseMs, 100, 0, USAGE_CLEANUP_MAX_PAUSE_MS)
  const cleanupRangeText = (cutoffLabel: string) => (
    parsedOlderThanDays === 0
      ? `${cutoffLabel}早于任务启动时刻（清理当时之前全部匹配记录）`
      : `${cutoffLabel}早于 ${parsedOlderThanDays} 天`
  )
  const payload = (): UsageCleanupRequest => ({
    mode,
    olderThanDays: parsedOlderThanDays,
    batchSize: parsedBatchSize,
    pauseMsBetweenBatches: parsedPauseMs,
  })

  const running = ['queued', 'running'].includes(cleanupStatus.data?.status || '')
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
      `确定开始${cleanupModeLabel(mode)}？\n\n范围：${cleanupRangeText(cutoffLabel)}\n每批：${formatNumber(parsedBatchSize)} 条\n系统会持续分批执行，直到没有更多匹配记录或本次执行达到安全上限 ${formatNumber(USAGE_CLEANUP_DEFAULT_MAX_BATCHES)} 批；达到上限后可显式恢复下一轮。\n\n软删除会同步扣除命中记录对应的顶部统计、费用和 Dashboard rollup；硬删除只物理删除已软删的记录。`
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

  const resume = () => {
    const jobId = cleanupStatus.data?.jobId
    if (!jobId) return
    resumeCleanup.mutate(jobId, {
      onSuccess: () => toast.success('Usage 清理任务已重新排队'),
      onError: (error) => toast.error(`恢复失败: ${extractErrorMessage(error)}`),
    })
  }

  const clearAll = () => {
    const confirmed = confirm('将提交后台任务，分批软删除全部 Usage 明细，并同步扣除这些记录对应的累计统计、费用和 Dashboard 汇总。任务可取消并审计。确认继续？')
    if (!confirmed) return
    clearRecords.mutate(undefined, {
      onSuccess: () => {
        toast.success('全量 Usage 明细清理任务已提交')
      },
      onError: (error) => toast.error(`清空失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>分批清理 Usage 记录</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 text-sm">
          <div className="rounded-md border border-destructive/30 bg-destructive/5 p-3">
            <div className="text-xs font-semibold text-destructive">危险操作</div>
            <p className="mt-1 text-xs leading-5 text-muted-foreground">
              后台分批软删除全部历史明细，并同步扣除对应的累计统计、费用和 Dashboard 汇总。任务状态会持久化，可取消并审计。
            </p>
            <Button
              className="mt-3 w-full text-destructive"
              variant="outline"
              onClick={clearAll}
              disabled={clearRecords.isPending || running}
            >
              {running ? '清理任务执行中' : clearRecords.isPending ? '提交中...' : '清理全部历史明细'}
            </Button>
          </div>

          <div className="rounded-md border border-kiro-warning-soft bg-kiro-warning-soft p-3 text-kiro-warning">
            这是手动任务，不会定时执行。系统会自动分批清理；每次执行最多 {formatNumber(USAGE_CLEANUP_DEFAULT_MAX_BATCHES)} 批，达到上限后暂停并等待管理员显式恢复。软删除会同步扣除命中记录对应的顶部统计、费用和 Dashboard rollup；硬删除只物理删除已软删的记录。
          </div>

          <div className="grid gap-3 md:grid-cols-2">
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">清理方式</span>
              <select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" value={mode} onChange={(event) => setMode(event.target.value as UsageCleanupMode)}>
                <option value="soft_delete">软删除可见明细</option>
                <option value="hard_delete">硬删除已软删记录</option>
              </select>
            </label>
            <div className="space-y-1">
              <span className="block text-xs text-muted-foreground">{mode === 'hard_delete' ? '删除时间早于多少天' : '创建时间早于多少天'}</span>
              <Input value={olderThanDays} onChange={(event) => setOlderThanDays(event.target.value)} inputMode="numeric" min={0} max={USAGE_CLEANUP_MAX_OLDER_THAN_DAYS} type="number" />
              <span className="block text-[0.68rem] text-muted-foreground">填 0 表示以任务启动时刻为 cutoff，最大 {formatNumber(USAGE_CLEANUP_MAX_OLDER_THAN_DAYS)} 天。</span>
            </div>
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">每批数量</span>
              <Input value={batchSize} onChange={(event) => setBatchSize(event.target.value)} inputMode="numeric" min={1} max={USAGE_CLEANUP_MAX_BATCH_SIZE} type="number" />
              <span className="block text-[0.68rem] text-muted-foreground">后端安全上限 {formatNumber(USAGE_CLEANUP_MAX_BATCH_SIZE)}。</span>
            </label>
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">批次间隔毫秒</span>
              <Input value={pauseMs} onChange={(event) => setPauseMs(event.target.value)} inputMode="numeric" min={0} max={USAGE_CLEANUP_MAX_PAUSE_MS} type="number" />
              <span className="block text-[0.68rem] text-muted-foreground">后端安全上限 {formatNumber(USAGE_CLEANUP_MAX_PAUSE_MS)}ms。</span>
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
                <span>阶段 {cleanupStatus.data.phase}</span>
                <span>剩余 {cleanupStatus.data.remainingRows === undefined ? '未知' : `${formatNumber(cleanupStatus.data.remainingRows)} 条`}</span>
                <span>累计执行 {formatNumber(cleanupStatus.data.batches)} 批</span>
                <span>单次执行上限 {formatNumber(cleanupStatus.data.maxBatches)} 批</span>
                <span>最后一批 {formatNumber(cleanupStatus.data.lastBatchRows)} 条</span>
                {cleanupStatus.data.redisDeleteCommands > 0 && <span>Redis {formatNumber(cleanupStatus.data.redisDeletedKeys)} keys / {formatNumber(cleanupStatus.data.redisDeleteCommands)} commands</span>}
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
            <Button variant="outline" onClick={resume} disabled={!['paused', 'failed', 'cancelled'].includes(cleanupStatus.data?.status || '') || resumeCleanup.isPending}>
              {resumeCleanup.isPending ? '恢复中...' : '恢复任务'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
