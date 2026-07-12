import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'
import { formatDate, formatNumber } from '@/lib/format'
import type { UsageRecord } from '@/types/api'
import { Badge } from '@/components/ui'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'
import { ModalShell } from '@/components/patterns'
import {
  formatAttemptChain,
  formatAttemptSummary,
  formatExternalAttemptChain,
  formatLatency,
  attemptActionLabel,
  routeLabel,
  routeTone,
  resolvedModelLabel,
  sourceLabel,
  statusLabel,
  statusTone,
  upstreamModelLabel,
} from './usage-helpers'
import { UsageCostBreakdown, UsageCostTiles, usageRecordCostModel } from './usage-billing'

function MetricTile({
  label,
  value,
  tone = 'default',
}: {
  label: string
  value: string
  tone?: 'default' | 'success' | 'info' | 'warning' | 'error'
}) {
  const cls = {
    default: 'text-foreground',
    success: 'text-success',
    info: 'text-primary',
    warning: 'text-warning',
    error: 'text-destructive',
  }[tone]
  return (
    <div className="rounded-lg bg-muted/30 px-2.5 py-1.5">
      <div className="text-[0.68rem] font-medium text-muted-foreground">{label}</div>
      <div className={cn('mt-0.5 truncate font-mono text-[0.82rem] font-semibold', cls)}>{value}</div>
    </div>
  )
}

function DetailField({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className={cn('break-all text-sm', mono && 'font-mono')}>{value || '-'}</div>
    </div>
  )
}

function SectionTitle({ children }: { children: ReactNode }) {
  return <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">{children}</div>
}

function formatJsonBlock(value: unknown): string {
  if (!value) return '-'
  try { return JSON.stringify(value, null, 2) } catch { return String(value) }
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

export function UsageDetailModal({
  record,
  open,
  onClose,
}: {
  record: UsageRecord | null
  open: boolean
  onClose: () => void
}) {
  if (!record) return null

  const hasLocalAttempts = (record.credentialAttempts?.length ?? 0) > 0
  const hasExternalAttempts = (record.externalAttempts?.length ?? 0) > 0
  const costModel = usageRecordCostModel(record)
  const showExternalResolvedModel =
    record.routeKind === 'external_pool'
    && !!record.upstreamModel
    && record.upstreamModel !== (record.externalOutboundModel || record.upstreamModel)

  return (
    <ModalShell open={open} onClose={onClose} title="请求明细" width="max-w-4xl">
      <div className="space-y-5 text-sm">
        {/* 基础信息 */}
        <div>
          <SectionTitle>基础信息</SectionTitle>
          <div className="grid gap-3 sm:grid-cols-2">
            <DetailField label="时间" value={formatDate(record.createdAt)} />
            <DetailField label="请求 ID" value={record.id} mono />
            <DetailField label="入口" value={record.endpoint} mono />
            <DetailField label="会话 ID" value={record.conversationId || '-'} mono />
            <DetailField label="模型（请求）" value={record.model} />
            <DetailField label="模型（上游）" value={upstreamModelLabel(record)} />
            {showExternalResolvedModel && (
              <DetailField label="模型（本地解析）" value={resolvedModelLabel(record)} />
            )}
            {record.modelResolutionNote && (
              <div className="sm:col-span-2">
                <DetailField label="模型解析说明" value={record.modelResolutionNote} />
              </div>
            )}
            <DetailField label="账号" value={record.credentialId != null ? `#${record.credentialId}${record.credentialLabel ? ' ' + record.credentialLabel : ''}` : '-'} />
            {record.routeKind === 'external_pool' && (
              <DetailField label="外部账号" value={`#${record.externalPoolId ?? '-'}${record.externalPoolName ? ' ' + record.externalPoolName : ''}`} />
            )}
            <DetailField
              label="路由"
              value={`${routeLabel(record)} · ${record.routeKind || '-'}${record.routeSubtype ? ` · ${record.routeSubtype}` : ''}`}
            />
          </div>
        </div>

        {/* 状态与路由 */}
        <div>
          <SectionTitle>状态与路由</SectionTitle>
          <div className="flex flex-wrap gap-2 mb-3">
            <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
            <Badge tone={routeTone(record)}>{routeLabel(record)}</Badge>
            <Badge tone={record.stream ? 'info' : 'neutral'}>{record.stream ? 'stream' : 'non-stream'}</Badge>
            {record.simulated && <Badge tone="warning">模拟</Badge>}
            {record.stickyBound && <Badge tone="primary">Sticky</Badge>}
            {record.fallbackFromSticky && <Badge tone="warning">Sticky 回退</Badge>}
            <Badge tone="neutral">{sourceLabel(record.usageSource)}</Badge>
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            {record.publicErrorType && <DetailField label="客户端错误类型" value={record.publicErrorType} />}
            {record.publicErrorStatusCode != null && <DetailField label="客户端状态码" value={String(record.publicErrorStatusCode)} />}
            {record.publicErrorMessage && <DetailField label="客户端收到的错误" value={record.publicErrorMessage} />}
            {record.errorType && <DetailField label="错误类型" value={record.errorType} />}
            {record.errorStatusCode != null && <DetailField label="内部状态码" value={String(record.errorStatusCode)} />}
            {record.errorSource && <DetailField label="错误阶段" value={record.errorSource} />}
            {record.errorId && <DetailField label="错误 ID" value={record.errorId} mono />}
            {record.errorMessage && <DetailField label="内部错误信息" value={record.errorMessage} />}
            {record.fallbackReason && <DetailField label="Fallback 原因" value={record.fallbackReason} />}
            {record.directPolicyReason && <DetailField label="直连原因" value={record.directPolicyReason} />}
          </div>
        </div>

        {/* 用量口径 */}
        <div>
          <SectionTitle>用量口径</SectionTitle>
          <div className="mb-1.5 text-xs text-muted-foreground">本地估算输入仅用于诊断；返回给客户端的用量以展示字段为准。</div>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
            <MetricTile label="本地估算输入" value={formatNumber(record.totalInputTokens)} tone="info" />
            <MetricTile label="展示输入" value={formatNumber(record.compatInputTokens)} />
            <MetricTile label="展示缓存写入" value={formatNumber(record.cacheCreationInputTokens)} tone="info" />
            <MetricTile label="展示缓存读取" value={formatNumber(record.cacheReadInputTokens)} tone="success" />
            <MetricTile label="展示输出" value={formatNumber(record.outputTokens)} />
            <MetricTile label="成本估算输入" value={formatNumber(record.billableInputTokens)} />
          </div>
          {(record.cacheCreation5mInputTokens > 0 || record.cacheCreation1hInputTokens > 0) && (
            <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-3">
              <MetricTile label="缓存写入·5分钟" value={formatNumber(record.cacheCreation5mInputTokens)} />
              <MetricTile label="缓存写入·1小时" value={formatNumber(record.cacheCreation1hInputTokens)} />
            </div>
          )}
          <div className="mt-2 text-xs leading-5 text-muted-foreground">
            成本估算输入 = 展示输入 + 展示缓存写入，仅用于本系统费用估算和历史兼容，不是返回给客户端的独立字段。
          </div>
          <div className="mt-2">
            <MetricTile label="用量来源" value={sourceLabel(record.usageSource)} />
            <UsageCostTiles model={costModel} className="mt-2" />
          </div>
        </div>

        <UsageCostBreakdown model={costModel} />

        {/* 耗时 */}
        <div>
          <SectionTitle>耗时</SectionTitle>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4">
            <MetricTile label="总耗时" value={formatLatency(record.durationMs)} />
            <MetricTile label="首 Token" value={formatLatency(record.firstTokenLatencyMs)} tone="info" />
            <MetricTile label="响应延迟" value={formatLatency(record.responseLatencyMs)} />
            {record.latencyTrace && (
              <>
                <MetricTile label="请求检查" value={formatLatency(record.latencyTrace.payloadGuardMs)} />
                <MetricTile label="上游响应头" value={formatLatency(record.latencyTrace.upstreamHeaderMs)} />
                <MetricTile label="首个流分片" value={formatLatency(record.latencyTrace.firstUpstreamChunkMs)} />
                <MetricTile label="首次输出" value={formatLatency(record.latencyTrace.firstOutputDeltaMs)} tone="success" />
                <MetricTile label="首次思考" value={formatLatency(record.latencyTrace.firstThinkingDeltaMs)} />
                <MetricTile label="首次可见文本" value={formatLatency(record.latencyTrace.firstVisibleTextDeltaMs)} tone="success" />
                <MetricTile label="分片到输出" value={formatLatency(record.latencyTrace.streamGapToFirstOutputMs)} />
                {typeof record.latencyTrace.capacityWeightUnits === 'number' && (
                  <MetricTile label="本地容量权重" value={`${formatNumber(record.latencyTrace.capacityWeightUnits)} 单位`} tone="info" />
                )}
                {typeof record.latencyTrace.estimatedInputTokens === 'number' && (
                  <MetricTile label="权重估算输入" value={`${formatNumber(record.latencyTrace.estimatedInputTokens)} token`} />
                )}
                {typeof record.latencyTrace.chunksBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前分片" value={formatNumber(record.latencyTrace.chunksBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.eventsBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前事件" value={formatNumber(record.latencyTrace.eventsBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.upstreamBytesBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前上游字节" value={formatNumber(record.latencyTrace.upstreamBytesBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.upstreamFramesBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前上游帧" value={formatNumber(record.latencyTrace.upstreamFramesBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.upstreamEventsBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前上游事件" value={formatNumber(record.latencyTrace.upstreamEventsBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.upstreamFramesWithoutDownstreamEventsBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前空转换帧" value={formatNumber(record.latencyTrace.upstreamFramesWithoutDownstreamEventsBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.upstreamPendingChunksBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前待解码分片" value={formatNumber(record.latencyTrace.upstreamPendingChunksBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.upstreamFrameDecodeErrorsBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前帧解码错" value={formatNumber(record.latencyTrace.upstreamFrameDecodeErrorsBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.upstreamEventParseErrorsBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前事件解析错" value={formatNumber(record.latencyTrace.upstreamEventParseErrorsBeforeFirstOutput)} />
                )}
                {record.latencyTrace.upstreamEventTypesBeforeFirstOutput && (
                  <MetricTile label="输出前上游类型" value={formatUpstreamEventTypeCounts(record.latencyTrace.upstreamEventTypesBeforeFirstOutput)} />
                )}
                {typeof record.latencyTrace.clientDroppedMs === 'number' && (
                  <MetricTile label="客户端断开" value={formatLatency(record.latencyTrace.clientDroppedMs)} />
                )}
                {record.latencyTrace.terminalReason && (
                  <MetricTile label="结束原因" value={record.latencyTrace.terminalReason} />
                )}
                {record.latencyTrace.upstreamMessageStatus && (
                  <MetricTile label="上游完成状态" value={record.latencyTrace.upstreamMessageStatus} tone={record.latencyTrace.sawUpstreamCompleted ? 'success' : 'info'} />
                )}
                {typeof record.latencyTrace.sawUpstreamCompleted === 'boolean' && (
                  <MetricTile label="上游显式完成" value={record.latencyTrace.sawUpstreamCompleted ? '是' : '否'} tone={record.latencyTrace.sawUpstreamCompleted ? 'success' : 'warning'} />
                )}
                {record.latencyTrace.stopReasonSource && (
                  <MetricTile label="结束原因来源" value={record.latencyTrace.stopReasonSource} />
                )}
                {record.latencyTrace.suspectedIntentPreambleEndTurn && (
                  <MetricTile label="疑似开场白空转" value="是" tone="warning" />
                )}
              </>
            )}
          </div>
        </div>

        {/* 本地调用链路 Table */}
        {hasLocalAttempts && (
          <div>
            <SectionTitle>本地调用链路</SectionTitle>
            <div className="mb-2 flex flex-wrap items-center gap-2">
              <Badge tone="secondary">{formatAttemptSummary(record)}</Badge>
            </div>
            <div className="mb-2 rounded-lg bg-muted/30 px-3 py-2 font-mono text-xs break-all">
              {formatAttemptChain(record) || '-'}
            </div>
            <div className="scrollbar-thin overflow-x-auto">
              <Table className="min-w-[640px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>顺序</TableHead>
                    <TableHead>账号</TableHead>
                    <TableHead>模型</TableHead>
                    <TableHead>状态</TableHead>
                    <TableHead>动作</TableHead>
                    <TableHead className="text-right">耗时</TableHead>
                    <TableHead>错误</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {record.credentialAttempts?.map((a) => (
                    <TableRow key={`${a.attempt}-${a.credentialId}-${a.durationMs}`}>
                      <TableCell>{a.attempt}</TableCell>
                      <TableCell>
                        <div className="font-medium">#{a.credentialId}</div>
                        {a.credentialLabel && (
                          <div className="max-w-[180px] truncate text-[0.68rem] text-muted-foreground/70">{a.credentialLabel}</div>
                        )}
                      </TableCell>
                      <TableCell className="max-w-[160px] truncate text-xs">{a.model || '-'}</TableCell>
                      <TableCell>{a.statusText || (a.status != null ? String(a.status) : '-')}</TableCell>
                      <TableCell>{attemptActionLabel(a.action)}</TableCell>
                      <TableCell className="text-right font-mono text-xs">{formatLatency(a.durationMs)}</TableCell>
                      <TableCell>
                        <div className="max-w-[260px] truncate text-xs" title={a.errorMessage || a.errorType || ''}>
                          {a.errorMessage || a.errorType || '-'}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          </div>
        )}

        {/* 外部池链路 Table */}
        {hasExternalAttempts && (
          <div>
            <SectionTitle>外部池链路</SectionTitle>
            <div className="mb-2 rounded-lg bg-muted/30 px-3 py-2 font-mono text-xs break-all">
              {formatExternalAttemptChain(record) || '-'}
            </div>
            <div className="scrollbar-thin overflow-x-auto">
              <Table className="min-w-[560px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>顺序</TableHead>
                    <TableHead>外部账号</TableHead>
                    <TableHead>请求模型</TableHead>
                    <TableHead>状态</TableHead>
                    <TableHead>动作</TableHead>
                    <TableHead className="text-right">耗时</TableHead>
                    <TableHead>错误</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {record.externalAttempts?.map((a) => (
                    <TableRow key={`${a.attempt}-${a.poolId}-${a.durationMs}`}>
                      <TableCell>{a.attempt}</TableCell>
                      <TableCell>
                        <div className="font-medium">#{a.poolId}</div>
                        <div className="max-w-[180px] truncate text-[0.68rem] text-muted-foreground/70">{a.poolName}</div>
                      </TableCell>
                      <TableCell>
                        <div className="max-w-[220px] truncate font-mono text-xs" title={a.outboundModel || ''}>
                          {a.outboundModel || '-'}
                        </div>
                      </TableCell>
                      <TableCell>{a.status != null ? String(a.status) : '-'}</TableCell>
                      <TableCell>{attemptActionLabel(a.action)}</TableCell>
                      <TableCell className="text-right font-mono text-xs">{formatLatency(a.durationMs)}</TableCell>
                      <TableCell>
                        <div className="max-w-[260px] truncate text-xs" title={a.errorMessage || a.errorType || ''}>
                          {a.errorMessage || a.errorType || '-'}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          </div>
        )}

        {/* 错误详情 */}
        <div>
          <SectionTitle>内部错误详情</SectionTitle>
          <pre className="max-h-60 overflow-auto rounded-lg bg-muted/30 p-3 text-xs whitespace-pre-wrap break-words">
            {record.errorDetail || record.errorMessage || '-'}
          </pre>
        </div>

        {record.errorMetadata != null && (
          <div>
            <SectionTitle>错误元数据</SectionTitle>
            <pre className="max-h-72 overflow-auto rounded-lg bg-muted/30 p-3 text-xs whitespace-pre-wrap break-words">
              {formatJsonBlock(record.errorMetadata)}
            </pre>
          </div>
        )}

        {/* 请求内容诊断 */}
        {Boolean(record.payloadBreakdown || record.payloadGuardReport) && (
          <div>
            <SectionTitle>请求内容诊断</SectionTitle>
            <pre className="max-h-72 overflow-auto rounded-lg bg-muted/30 p-3 text-xs whitespace-pre-wrap break-words">
              {formatJsonBlock({
                breakdown: record.payloadBreakdown || null,
                guard: record.payloadGuardReport || null,
              })}
            </pre>
          </div>
        )}
      </div>
    </ModalShell>
  )
}
