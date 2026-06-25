import { cn } from '@/lib/utils'
import { formatDate, formatNumber, formatUsd } from '@/lib/format'
import type { UsageRecord } from '@/types/api'
import { Badge } from '@/components/ui'
import { ModalShell } from '@/components/patterns'
import {
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
    <div className="rounded-lg border border-border bg-muted/40 px-2.5 py-1.5">
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

function SectionTitle({ children }: { children: React.ReactNode }) {
  return <div className="mb-2 text-xs font-semibold text-muted-foreground uppercase tracking-wide">{children}</div>
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
  const hasExternalBilling = !!record.externalPoolBilling

  return (
    <ModalShell open={open} onClose={onClose} title="请求明细" width="max-w-3xl">
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
          </div>
        </div>

        {/* 状态与路由 */}
        <div>
          <SectionTitle>状态与路由</SectionTitle>
          <div className="flex flex-wrap gap-2 mb-3">
            <Badge tone={statusTone(record.status)}>{statusLabel(record.status)}</Badge>
            <Badge tone={routeTone(record)}>{routeLabel(record)}</Badge>
            {record.stream && <Badge tone="info">流式</Badge>}
            {record.simulated && <Badge tone="neutral">模拟</Badge>}
            {record.stickyBound && <Badge tone="primary">Sticky</Badge>}
            {record.fallbackFromSticky && <Badge tone="warning">Sticky 回退</Badge>}
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            {record.errorType && <DetailField label="错误类型" value={record.errorType} />}
            {record.errorMessage && <DetailField label="错误信息" value={record.errorMessage} />}
            {record.fallbackReason && <DetailField label="Fallback 原因" value={record.fallbackReason} />}
          </div>
        </div>

        {/* Token 计量 */}
        <div>
          <SectionTitle>Token 计量</SectionTitle>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4">
            <MetricTile label="输入 Token" value={formatNumber(record.totalInputTokens)} tone="info" />
            <MetricTile label="输出 Token" value={formatNumber(record.outputTokens)} tone="success" />
            <MetricTile label="缓存读取" value={formatNumber(record.cacheReadInputTokens)} />
            <MetricTile label="缓存写入" value={formatNumber(record.cacheCreationInputTokens)} />
            <MetricTile label="可计费输入" value={formatNumber(record.billableInputTokens)} />
            <MetricTile label="用量来源" value={sourceLabel(record.usageSource)} />
            <MetricTile label="估算费用" value={formatUsd(record.estimatedCostUsd)} tone={record.estimatedCostUsd > 0 ? 'warning' : 'default'} />
            <MetricTile label="有定价" value={record.pricingAvailable ? '是' : '否'} />
          </div>
        </div>

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
              </>
            )}
          </div>
        </div>

        {/* 本地调度链路 */}
        {hasLocalAttempts && (
          <div>
            <SectionTitle>本地调度链路</SectionTitle>
            <div className="mb-2 text-xs text-muted-foreground">{formatAttemptSummary(record)}</div>
            <div className="rounded-lg border border-border bg-muted/40 p-2.5 font-mono text-xs break-all">
              {formatAttemptChain(record) || '-'}
            </div>
            <div className="mt-2 space-y-1">
              {record.credentialAttempts?.map((a) => (
                <div key={a.attempt} className="flex items-center gap-2 text-xs">
                  <span className="w-5 text-right text-muted-foreground/60">#{a.attempt}</span>
                  <span className="font-mono text-muted-foreground">账号 {a.credentialId}</span>
                  {a.credentialLabel && <span className="truncate text-foreground">{a.credentialLabel}</span>}
                  <span className="ml-auto shrink-0 text-muted-foreground">{formatLatency(a.durationMs)}</span>
                  <Badge tone={a.action === 'success' ? 'success' : 'error'} className="shrink-0">
                    {a.status ?? a.errorType ?? a.action}
                  </Badge>
                </div>
              ))}
            </div>
          </div>
        )}

        {/* 外部池链路 */}
        {hasExternalAttempts && (
          <div>
            <SectionTitle>外部池链路</SectionTitle>
            <div className="rounded-lg border border-border bg-muted/40 p-2.5 font-mono text-xs break-all mb-2">
              {formatExternalAttemptChain(record) || '-'}
            </div>
            <div className="space-y-1">
              {record.externalAttempts?.map((a) => (
                <div key={a.attempt} className="flex items-center gap-2 text-xs">
                  <span className="w-5 text-right text-muted-foreground/60">#{a.attempt}</span>
                  <span className="font-mono text-muted-foreground">外部池 {a.poolId}</span>
                  <span className="truncate text-foreground">{a.poolName}</span>
                  <span className="ml-auto shrink-0 text-muted-foreground">{formatLatency(a.durationMs)}</span>
                  <Badge tone={a.action === 'success' ? 'success' : 'error'} className="shrink-0">
                    {a.status ?? a.errorType ?? a.action}
                  </Badge>
                </div>
              ))}
            </div>
          </div>
        )}

        {/* 外部池计费 */}
        {hasExternalBilling && record.externalPoolBilling && (
          <div>
            <SectionTitle>外部池计费</SectionTitle>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
              <MetricTile label="原始费用" value={formatUsd(record.externalPoolBilling.rawCostUsd)} />
              <MetricTile label="上报费用" value={formatUsd(record.externalPoolBilling.reportedCostUsd)} />
              <MetricTile label="可计费费用" value={formatUsd(record.externalPoolBilling.billableCostUsd)} />
              {record.externalPoolBilling.profitUsd !== undefined && (
                <MetricTile
                  label="盈亏"
                  value={formatUsd(record.externalPoolBilling.profitUsd)}
                  tone={record.externalPoolBilling.profitUsd >= 0 ? 'success' : 'error'}
                />
              )}
            </div>
          </div>
        )}
      </div>
    </ModalShell>
  )
}
