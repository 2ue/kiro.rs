import { Eye, LayoutGrid, List, Trash2, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button, Card, Input, Select, Table } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, LoadingState, ModalShell, SectionCard, StatCard } from '@/components/common'
import { formatDate, formatNumber, formatPercent, formatUsd, ratio } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import { useCredentials } from '@/hooks/use-credentials'
import {
  useClearUsageRecords,
  useUsageRecordsPage,
  useUsageSummary,
} from '@/hooks/use-usage'
import type { UsageRecord, UsageRecordsPageQuery, UsageRecordStatus, UsageSource } from '@/types/api'

function sourceLabel(source: UsageSource): string {
  const labels: Record<UsageSource, string> = {
    upstream_metadata: '上游 metadata',
    local_prompt_cache: '本地 prompt cache',
    context_estimate: '上下文估算',
    request_estimate: '请求估算',
    none: '无缓存',
  }
  return labels[source] || source
}

function statusLabel(status: string): string {
  const labels: Record<string, string> = {
    success: '成功',
    error: '错误',
    stream_error: '流错误',
    upstream_timeout: '上游超时',
    client_dropped: '客户端断开',
  }
  return labels[status] || status
}

function statusTone(status: string): 'success' | 'warning' | 'error' {
  if (status === 'success') return 'success'
  if (status === 'client_dropped') return 'warning'
  return 'error'
}

function attemptActionLabel(action: string): string {
  const labels: Record<string, string> = {
    success: '成功',
    retry: '重试',
    transient_retry: '重试',
    fail: '失败',
    disable_and_retry: '禁用后重试',
    failure_count_and_retry: '计失败后重试',
    force_refresh_and_retry: '刷新后重试',
  }
  return labels[action] || action || '-'
}

function attemptOutcomeLabel(record: NonNullable<UsageRecord['credentialAttempts']>[number]): string {
  if (typeof record.status === 'number') return String(record.status)
  if (record.errorType) return record.errorType
  return attemptActionLabel(record.action)
}

function formatAttemptChain(record: UsageRecord): string {
  return (record.credentialAttempts || [])
    .map((attempt) => `#${attempt.credentialId}(${attemptOutcomeLabel(attempt)})`)
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
  tone?: 'default' | 'success' | 'info'
}) {
  const toneClass = tone === 'success' ? 'text-success' : tone === 'info' ? 'text-primary' : 'text-base-content'
  return (
    <div className="rounded-box border border-base-300/60 bg-base-100 px-2.5 py-1.5">
      <div className="text-[0.68rem] font-medium text-base-content/50">{label}</div>
      <div className={`mt-0.5 truncate font-mono text-[0.82rem] font-semibold ${toneClass}`}>{value}</div>
    </div>
  )
}

export function UsagePanel() {
  const [searchText, setSearchText] = useState('')
  const [model, setModel] = useState('')
  const [conversationId, setConversationId] = useState('')
  const [credentialId, setCredentialId] = useState('')
  const [status, setStatus] = useState<UsageRecordStatus | ''>('')
  const [source, setSource] = useState<UsageSource | ''>('')
  const [streamMode, setStreamMode] = useState<'all' | 'stream' | 'non_stream'>('all')
  const [minCacheRead, setMinCacheRead] = useState('')
  const [selectedRecord, setSelectedRecord] = useState<UsageRecord | null>(null)
  const [recordView, setRecordView] = useState<'cards' | 'table'>('cards')
  const [page, setPage] = useState(1)
  const limit = 20

  const query = useMemo<UsageRecordsPageQuery>(() => {
    const next: UsageRecordsPageQuery = { page, limit }
    if (searchText.trim()) next.q = searchText.trim()
    if (model.trim()) next.model = model.trim()
    if (conversationId.trim()) next.conversationId = conversationId.trim()
    if (credentialId.trim() && Number.isFinite(Number(credentialId))) next.credentialId = Number(credentialId)
    if (status) next.status = status
    if (source) next.source = source
    if (streamMode !== 'all') next.stream = streamMode === 'stream'
    if (minCacheRead.trim() && Number.isFinite(Number(minCacheRead))) next.minCacheRead = Number(minCacheRead)
    return next
  }, [conversationId, credentialId, minCacheRead, model, page, searchText, source, status, streamMode])

  const summary = useUsageSummary()
  const records = useUsageRecordsPage(query)
  const credentials = useCredentials()
  const clearRecords = useClearUsageRecords()

  useEffect(() => {
    setPage(1)
  }, [conversationId, credentialId, minCacheRead, model, searchText, source, status, streamMode])

  const credentialLabels = useMemo(() => {
    const labels = new Map<number, string>()
    for (const credential of credentials.data?.credentials || []) {
      labels.set(credential.id, credential.email || credential.maskedApiKey || `凭据 #${credential.id}`)
    }
    return labels
  }, [credentials.data?.credentials])

  const hasFilters = Boolean(searchText || model || conversationId || credentialId || status || source || streamMode !== 'all' || minCacheRead)
  const pageRecords = records.data?.records || []
  const hasNext = Boolean(records.data?.hasNext)
  const summaryData = summary.data
  const readRatio = ratio(summaryData?.localPromptCacheReadInputTokens || 0, summaryData?.localPromptCacheInputTokens || 0)
  const cachedRatio = ratio(
    (summaryData?.localPromptCacheReadInputTokens || 0) + (summaryData?.localPromptCacheCreationInputTokens || 0),
    summaryData?.localPromptCacheInputTokens || 0
  )
  const pricedRatio = ratio(summaryData?.pricedRequests || 0, summaryData?.totalRequests || 0)
  const realtime = summaryData?.realtime
  const realtimeWindow = realtime?.windowSeconds || 60

  const resetFilters = () => {
    setSearchText('')
    setModel('')
    setConversationId('')
    setCredentialId('')
    setStatus('')
    setSource('')
    setStreamMode('all')
    setMinCacheRead('')
  }

  const clear = () => {
    if (!confirm('确定清空 Usage 展示记录吗？历史行仍保留用于审计。')) return
    clearRecords.mutate(undefined, {
      onSuccess: (res) => toast.success(res.message),
      onError: (error) => toast.error(`清空失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <div className="space-y-4">
      <div className="metric-grid">
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
          desc={`计费 ${formatNumber(realtime?.billableTpm || 0)}`}
          tone="info"
        />
        <StatCard title="高缓存请求" value={formatNumber(summaryData?.highCacheRequests || 0)} tone="success" />
        <StatCard title="缓存读取" value={formatNumber(summaryData?.totalCacheReadInputTokens || 0)} desc={`本地读取 ${formatPercent(readRatio)} / 总缓存 ${formatPercent(cachedRatio)}`} />
        <StatCard title="估算费用" value={formatUsd(summaryData?.totalEstimatedCostUsd || 0)} desc={`已计价 ${formatPercent(pricedRatio)}`} tone="info" />
      </div>

      <SectionCard
        title="使用记录"
        description="错误详情和账号切换链路可点击查看。"
        actions={
          <>
            <div className="join">
              <Button
                type="button"
                className="join-item"
                color={recordView === 'cards' ? 'primary' : 'ghost'}
                size="sm"
                onClick={() => setRecordView('cards')}
              >
                <LayoutGrid className="h-4 w-4" />
                卡片
              </Button>
              <Button
                type="button"
                className="join-item"
                color={recordView === 'table' ? 'primary' : 'ghost'}
                size="sm"
                onClick={() => setRecordView('table')}
              >
                <List className="h-4 w-4" />
                列表
              </Button>
            </div>
            <Button type="button" variant="outline" size="sm" onClick={resetFilters} disabled={!hasFilters}>
              <X className="h-4 w-4" />
              重置
            </Button>
            <Button type="button" color="error" variant="outline" size="sm" onClick={clear} disabled={clearRecords.isPending}>
              <Trash2 className="h-4 w-4" />
              清空
            </Button>
          </>
        }
      >
        <div className="mb-3 grid gap-2 md:grid-cols-2 xl:grid-cols-4">
          <Input bordered size="sm" className="xl:col-span-2" value={searchText} onChange={(event) => setSearchText(event.target.value)} placeholder="搜索模型、账号、会话、错误" />
          <Input bordered size="sm" value={model} onChange={(event) => setModel(event.target.value)} placeholder="模型" />
          <Input bordered size="sm" value={conversationId} onChange={(event) => setConversationId(event.target.value)} placeholder="会话 ID" />
          <Input bordered size="sm" value={credentialId} onChange={(event) => setCredentialId(event.target.value)} placeholder="账号 ID" inputMode="numeric" />
          <Select bordered size="sm" value={status} onChange={(event) => setStatus(event.target.value as UsageRecordStatus | '')}>
            <Select.Option value="">全部状态</Select.Option>
            <Select.Option value="success">成功</Select.Option>
            <Select.Option value="error">错误</Select.Option>
            <Select.Option value="stream_error">流错误</Select.Option>
            <Select.Option value="upstream_timeout">上游超时</Select.Option>
            <Select.Option value="client_dropped">客户端断开</Select.Option>
          </Select>
          <Select bordered size="sm" value={source} onChange={(event) => setSource(event.target.value as UsageSource | '')}>
            <Select.Option value="">全部来源</Select.Option>
            <Select.Option value="upstream_metadata">上游 metadata</Select.Option>
            <Select.Option value="local_prompt_cache">本地 prompt cache</Select.Option>
            <Select.Option value="context_estimate">上下文估算</Select.Option>
            <Select.Option value="request_estimate">请求估算</Select.Option>
            <Select.Option value="none">无缓存</Select.Option>
          </Select>
          <Select bordered size="sm" value={streamMode} onChange={(event) => setStreamMode(event.target.value as 'all' | 'stream' | 'non_stream')}>
            <Select.Option value="all">全部请求</Select.Option>
            <Select.Option value="stream">Stream</Select.Option>
            <Select.Option value="non_stream">非 Stream</Select.Option>
          </Select>
          <Input bordered size="sm" value={minCacheRead} onChange={(event) => setMinCacheRead(event.target.value)} placeholder="最小 cache read" inputMode="numeric" />
        </div>

        {records.isLoading ? (
          <LoadingState />
        ) : records.error ? (
          <ErrorState text={extractErrorMessage(records.error)} />
        ) : pageRecords.length === 0 ? (
          <EmptyState text={page === 1 ? '暂无记录' : '当前页暂无记录'} />
        ) : recordView === 'table' ? (
          <div className="table-panel">
            <Table size="sm" className="data-table min-w-[1120px]">
              <Table.Head>
                <span>时间 / 状态</span>
                <span>模型 / Endpoint</span>
                <span>账号</span>
                <span>Token</span>
                <span>缓存</span>
                <span>费用 / 耗时</span>
                <span>调用链路</span>
                <span className="text-right">操作</span>
              </Table.Head>
              <Table.Body>
                {pageRecords.map((record) => {
                  const label = typeof record.credentialId === 'number' ? credentialLabels.get(record.credentialId) || record.credentialLabel : record.credentialLabel
                  const rowReadRatio = ratio(record.cacheReadInputTokens, record.totalInputTokens)
                  const rowCachedRatio = ratio(record.cacheReadInputTokens + record.cacheCreationInputTokens, record.totalInputTokens)
                  const attemptChain = formatAttemptChain(record)
                  return (
                    <Table.Row key={record.id}>
                      <span>
                        <div className="font-medium text-base-content/75">{formatDate(record.createdAt)}</div>
                        <div className="mt-1 flex flex-wrap items-center gap-1">
                          <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
                          <Badge tone={record.stream ? 'secondary' : 'neutral'}>{record.stream ? 'stream' : 'non-stream'}</Badge>
                        </div>
                      </span>
                      <span className="min-w-0">
                        <div className="max-w-[260px] truncate font-semibold" title={record.model || '-'}>
                          {record.model || '-'}
                        </div>
                        <div className="mt-1 flex max-w-[260px] flex-wrap items-center gap-1">
                          <Badge>{record.endpoint || '-'}</Badge>
                          {record.stickyBound && <Badge tone="secondary">sticky</Badge>}
                          {record.fallbackFromSticky && <Badge tone="warning">fallback</Badge>}
                        </div>
                      </span>
                      <span>
                        <div className="font-semibold">#{record.credentialId ?? '-'}</div>
                        {label && <div className="max-w-[180px] truncate text-xs text-base-content/55" title={label}>{label}</div>}
                      </span>
                      <span className="font-mono text-xs">
                        <div>输入 {formatNumber(record.totalInputTokens)}</div>
                        <div className="text-base-content/55">计费 {formatNumber(record.billableInputTokens)}</div>
                        <div className="text-base-content/55">输出 {formatNumber(record.outputTokens)}</div>
                      </span>
                      <span className="font-mono text-xs">
                        <div className="text-success">读 {formatNumber(record.cacheReadInputTokens)}</div>
                        <div className="text-info">写 {formatNumber(record.cacheCreationInputTokens)}</div>
                        <div className="text-base-content/55">{formatPercent(rowReadRatio)} / {formatPercent(rowCachedRatio)}</div>
                        <div className="mt-1"><Badge tone={record.simulated ? 'warning' : 'secondary'}>{sourceLabel(record.usageSource)}</Badge></div>
                      </span>
                      <span>
                        <div className="font-semibold">{formatUsd(record.estimatedCostUsd || 0)}</div>
                        <div className="text-xs text-base-content/55">{formatNumber(record.durationMs)}ms</div>
                        <div className="max-w-[160px] truncate text-xs text-base-content/55" title={record.pricingModel || ''}>
                          {record.pricingAvailable ? record.pricingModel || 'priced' : 'unpriced'}
                        </div>
                      </span>
                      <span>
                        {attemptChain ? (
                          <button
                            type="button"
                            className="max-w-[220px] truncate text-left text-xs font-medium text-primary hover:underline"
                            title={attemptChain}
                            onClick={() => setSelectedRecord(record)}
                          >
                            {attemptChain}
                          </button>
                        ) : (
                          <span className="text-xs text-base-content/40">-</span>
                        )}
                        {record.errorMessage && (
                          <button
                            type="button"
                            className="mt-1 block max-w-[220px] truncate text-left text-xs text-error hover:underline"
                            title={record.errorDetail || record.errorMessage}
                            onClick={() => setSelectedRecord(record)}
                          >
                            {record.errorMessage}
                          </button>
                        )}
                      </span>
                      <span className="text-right">
                        <Button type="button" variant="outline" size="xs" onClick={() => setSelectedRecord(record)} title="查看详情">
                          <Eye className="h-3.5 w-3.5" />
                          详情
                        </Button>
                      </span>
                    </Table.Row>
                  )
                })}
              </Table.Body>
            </Table>
          </div>
        ) : (
          <div className="usage-record-list">
            {pageRecords.map((record) => {
              const label = typeof record.credentialId === 'number' ? credentialLabels.get(record.credentialId) || record.credentialLabel : record.credentialLabel
              const rowReadRatio = ratio(record.cacheReadInputTokens, record.totalInputTokens)
              const rowCachedRatio = ratio(record.cacheReadInputTokens + record.cacheCreationInputTokens, record.totalInputTokens)
              const attemptChain = formatAttemptChain(record)
              return (
                <Card key={record.id} className="usage-record-card">
                  <Card.Body className="gap-2.5 p-3">
                    <div className="flex flex-col gap-2 xl:flex-row xl:items-start xl:justify-between">
                      <div className="min-w-0">
                        <div className="flex flex-wrap items-center gap-1.5">
                          <span className="text-xs font-medium text-base-content/55">{formatDate(record.createdAt)}</span>
                          <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
                          <Badge tone={record.simulated ? 'warning' : 'secondary'}>{sourceLabel(record.usageSource)}</Badge>
                          <Badge tone={record.stream ? 'secondary' : 'neutral'}>{record.stream ? 'stream' : 'non-stream'}</Badge>
                        </div>
                        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                          <span className="max-w-[360px] truncate text-sm font-semibold" title={record.model || '-'}>
                            {record.model || '-'}
                          </span>
                          <Badge>{record.endpoint || '-'}</Badge>
                          {record.stickyBound && <Badge tone="secondary">sticky</Badge>}
                          {record.fallbackFromSticky && <Badge tone="warning">fallback</Badge>}
                        </div>
                      </div>
                      <div className="flex shrink-0 flex-wrap items-center gap-2 text-sm">
                        <div className="text-right">
                          <div className="font-semibold">{formatUsd(record.estimatedCostUsd || 0)}</div>
                          <div className="text-xs text-base-content/50">{record.pricingAvailable ? record.pricingModel || 'priced' : 'unpriced'}</div>
                        </div>
                        <div className="text-right">
                          <div className="font-semibold">{formatNumber(record.durationMs)}ms</div>
                          <div className="text-xs text-base-content/50">耗时</div>
                        </div>
                        <Button type="button" variant="outline" size="xs" onClick={() => setSelectedRecord(record)} title="查看详情">
                          <Eye className="h-3.5 w-3.5" />
                          详情
                        </Button>
                      </div>
                    </div>

                    <div className="grid gap-2 text-sm md:grid-cols-2 xl:grid-cols-[220px_1fr]">
                      <div className="min-w-0 rounded-box bg-base-200/60 px-2.5 py-1.5">
                        <div className="text-xs text-base-content/50">账号</div>
                        <div className="font-semibold">#{record.credentialId ?? '-'}</div>
                        {label && <div className="truncate text-xs text-base-content/60" title={label}>{label}</div>}
                      </div>
                      <div className="min-w-0 rounded-box bg-base-200/60 px-2.5 py-1.5">
                        <div className="text-xs text-base-content/50">会话</div>
                        <div className="truncate font-mono text-xs" title={record.conversationId || '-'}>{record.conversationId || '-'}</div>
                        {attemptChain && (
                          <button
                            type="button"
                            className="mt-1 max-w-full truncate text-left text-xs font-medium text-primary hover:underline"
                            title={attemptChain}
                            onClick={() => setSelectedRecord(record)}
                          >
                            调用链路 {attemptChain}
                          </button>
                        )}
                      </div>
                    </div>

                    <div className="usage-stat-grid">
                      <UsageMetric label="总输入" value={formatNumber(record.totalInputTokens)} />
                      <UsageMetric label="上报输入" value={formatNumber(record.compatInputTokens)} />
                      <UsageMetric label="计费输入" value={formatNumber(record.billableInputTokens)} />
                      <UsageMetric label="缓存读取" value={formatNumber(record.cacheReadInputTokens)} tone="success" />
                      <UsageMetric label="缓存写入" value={formatNumber(record.cacheCreationInputTokens)} tone="info" />
                      <UsageMetric label="读取率" value={formatPercent(rowReadRatio)} />
                      <UsageMetric label="缓存率" value={formatPercent(rowCachedRatio)} />
                      <UsageMetric label="输出" value={formatNumber(record.outputTokens)} />
                    </div>

                    {record.errorMessage && (
                      <button
                        type="button"
                        className="rounded-box border border-error/20 bg-error/5 px-2.5 py-1.5 text-left text-xs text-error hover:bg-error/10"
                        onClick={() => setSelectedRecord(record)}
                        title={record.errorDetail || record.errorMessage}
                      >
                        <span className="font-semibold">错误详情：</span>
                        <span className="line-clamp-2">{record.errorMessage}</span>
                      </button>
                    )}
                  </Card.Body>
                </Card>
              )
            })}
          </div>
        )}

        {(page > 1 || hasNext) && (
          <div className="mt-4 flex items-center justify-center gap-3">
            <Button type="button" variant="outline" size="sm" disabled={page === 1} onClick={() => setPage((value) => Math.max(1, value - 1))}>
              上一页
            </Button>
            <span className="text-sm text-base-content/60">第 {page} 页，每页 {limit} 条</span>
            <Button type="button" variant="outline" size="sm" disabled={!hasNext} onClick={() => setPage((value) => value + 1)}>
              下一页
            </Button>
          </div>
        )}
      </SectionCard>

      <UsageDetailModal record={selectedRecord} onClose={() => setSelectedRecord(null)} />
    </div>
  )
}

function UsageDetailModal({ record, onClose }: { record: UsageRecord | null; onClose: () => void }) {
  return (
    <ModalShell open={Boolean(record)} title="使用详情" width="max-w-5xl" onClose={onClose}>
      {record && (
        <div className="space-y-4">
          <div className="grid gap-3 text-sm md:grid-cols-2">
            <Detail label="请求 ID" value={record.id} mono />
            <Detail label="时间" value={formatDate(record.createdAt)} />
            <Detail label="模型" value={record.model || '-'} />
            {record.upstreamModel && record.upstreamModel !== record.model && (
              <Detail
                label="上游模型"
                value={`${record.upstreamModel}${record.modelResolutionSource ? `（${record.modelResolutionSource}）` : ''}`}
              />
            )}
            <Detail label="会话" value={record.conversationId || '-'} mono />
            <Detail label="账号" value={`#${record.credentialId ?? '-'} ${record.credentialLabel || ''}`} />
            <Detail label="状态" value={statusLabel(record.status)} />
            <Detail label="估算费用" value={`${formatUsd(record.estimatedCostUsd || 0)} ${record.pricingAvailable ? record.pricingModel || 'priced' : 'unpriced'}`} />
          </div>
          {(record.credentialAttempts || []).length > 0 && (
            <div>
              <div className="mb-2 text-sm font-medium">调用链路</div>
              <div className="mb-2 rounded-box border border-base-300 bg-base-200 px-3 py-2 font-mono text-xs">{formatAttemptChain(record)}</div>
              <div className="table-panel">
                <Table size="sm" className="data-table min-w-[760px]">
                  <Table.Head>
                    <span>顺序</span>
                    <span>账号</span>
                    <span>状态</span>
                    <span>动作</span>
                    <span className="text-right">耗时</span>
                    <span>错误</span>
                  </Table.Head>
                  <Table.Body>
                    {(record.credentialAttempts || []).map((attempt) => (
                      <Table.Row key={`${attempt.attempt}-${attempt.credentialId}-${attempt.durationMs}`}>
                        <span>{attempt.attempt}</span>
                        <span>
                          <div className="font-medium">#{attempt.credentialId}</div>
                          {attempt.credentialLabel && <div className="max-w-[220px] truncate text-xs text-base-content/60">{attempt.credentialLabel}</div>}
                          {attempt.model && <div className="max-w-[220px] truncate text-xs text-base-content/60">模型 {attempt.model}</div>}
                        </span>
                        <span>{attempt.statusText || attempt.status || '-'}</span>
                        <span>{attemptActionLabel(attempt.action)}</span>
                        <span className="text-right">{formatNumber(attempt.durationMs)}ms</span>
                        <span><div className="max-w-[320px] truncate" title={attempt.errorMessage || attempt.errorType || ''}>{attempt.errorMessage || attempt.errorType || '-'}</div></span>
                      </Table.Row>
                    ))}
                  </Table.Body>
                </Table>
              </div>
            </div>
          )}
          <div>
            <div className="mb-2 text-sm font-medium">错误详情</div>
            <pre className="max-h-96 overflow-auto rounded-box border border-base-300 bg-base-200 p-3 text-xs whitespace-pre-wrap break-words">
              {record.errorDetail || record.errorMessage || '-'}
            </pre>
          </div>
          {Boolean(record.payloadBreakdown || record.payloadGuardReport) && (
            <div>
              <div className="mb-2 text-sm font-medium">Payload 诊断</div>
              <pre className="max-h-96 overflow-auto rounded-box border border-base-300 bg-base-200 p-3 text-xs whitespace-pre-wrap break-words">
                {formatJsonBlock({
                  breakdown: record.payloadBreakdown || null,
                  guard: record.payloadGuardReport || null,
                })}
              </pre>
            </div>
          )}
        </div>
      )}
    </ModalShell>
  )
}

function Detail({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div>
      <div className="text-xs text-base-content/50">{label}</div>
      <div className={`break-all ${mono ? 'font-mono' : ''}`}>{value}</div>
    </div>
  )
}
