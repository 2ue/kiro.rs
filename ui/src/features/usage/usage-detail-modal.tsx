import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'
import { formatDate, formatNumber, formatUsd } from '@/lib/format'
import type { ExternalPoolUsageSnapshot, UsageRecord } from '@/types/api'
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

function SectionTitle({ children }: { children: ReactNode }) {
  return <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">{children}</div>
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

function formatJsonBlock(value: unknown): string {
  if (!value) return '-'
  try { return JSON.stringify(value, null, 2) } catch { return String(value) }
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
  const billing = record.externalPoolBilling

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
            {record.modelResolutionNote && (
              <div className="sm:col-span-2">
                <DetailField label="模型解析说明" value={record.modelResolutionNote} />
              </div>
            )}
            <DetailField label="账号" value={record.credentialId != null ? `#${record.credentialId}${record.credentialLabel ? ' ' + record.credentialLabel : ''}` : '-'} />
            {record.routeKind === 'external_pool' && (
              <DetailField label="外部账号" value={`#${record.externalPoolId ?? '-'}${record.externalPoolName ? ' ' + record.externalPoolName : ''}`} />
            )}
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
            {record.errorType && <DetailField label="错误类型" value={record.errorType} />}
            {record.errorMessage && <DetailField label="错误信息" value={record.errorMessage} />}
            {record.fallbackReason && <DetailField label="Fallback 原因" value={record.fallbackReason} />}
            {record.directPolicyReason && <DetailField label="直连原因" value={record.directPolicyReason} />}
          </div>
        </div>

        {/* 用量口径 */}
        <div>
          <SectionTitle>用量口径</SectionTitle>
          <div className="mb-1.5 text-xs text-muted-foreground">主列表只展示返回给客户端的用量；实际输入和成本估算只在详情里查看。</div>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
            <MetricTile label="用户实际输入" value={formatNumber(record.totalInputTokens)} tone="info" />
            <MetricTile label="展示输入" value={formatNumber(record.compatInputTokens)} />
            <MetricTile label="展示缓存写入" value={formatNumber(record.cacheCreationInputTokens)} tone="info" />
            <MetricTile label="展示缓存读取" value={formatNumber(record.cacheReadInputTokens)} tone="success" />
            <MetricTile label="展示输出" value={formatNumber(record.outputTokens)} />
            <MetricTile label="成本估算输入" value={formatNumber(record.billableInputTokens)} />
          </div>
          <div className="mt-2 text-xs leading-5 text-muted-foreground">
            成本估算输入 = 展示输入 + 展示缓存写入，仅用于本系统费用估算和历史兼容，不是返回给客户端的独立字段。
          </div>
          <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-3">
            <MetricTile label="用量来源" value={sourceLabel(record.usageSource)} />
            <MetricTile label="估算费用" value={formatUsd(record.estimatedCostUsd)} tone={record.estimatedCostUsd > 0 ? 'warning' : 'default'} />
            <MetricTile label="有定价" value={record.pricingAvailable ? `是（${record.pricingModel || 'priced'}）` : '否'} />
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
                {typeof record.latencyTrace.chunksBeforeFirstOutput === 'number' && (
                  <MetricTile label="输出前分片" value={formatNumber(record.latencyTrace.chunksBeforeFirstOutput)} />
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
            <div className="mb-2 rounded-lg border border-border bg-muted/40 px-3 py-2 font-mono text-xs break-all">
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
            <div className="mb-2 rounded-lg border border-border bg-muted/40 px-3 py-2 font-mono text-xs break-all">
              {formatExternalAttemptChain(record) || '-'}
            </div>
            <div className="scrollbar-thin overflow-x-auto">
              <Table className="min-w-[560px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>顺序</TableHead>
                    <TableHead>外部账号</TableHead>
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

        {/* 外部池计费 */}
        {hasExternalBilling && billing && (
          <div>
            <SectionTitle>外部池计费拆分</SectionTitle>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <div>
                <div className="text-xs text-muted-foreground">原始成本</div>
                <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.rawUsage)}</div>
                <div className="mt-1 font-medium">{formatUsd(billing.rawCostUsd)}</div>
                <div className="text-xs text-muted-foreground">按外部账号实际消耗估算</div>
              </div>
              <div>
                <div className="text-xs text-muted-foreground">展示计费（shaped）</div>
                <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.shapedUsage || billing.reportedUsage)}</div>
                <div className="mt-1 font-medium">{formatUsd(billing.shapedCostUsd ?? billing.reportedCostUsd ?? 0)}</div>
                <div className="text-xs text-muted-foreground">{billing.usageProjectionApplied ? '已按入口规则展示' : '保持原样'}</div>
              </div>
              <div>
                <div className="text-xs text-muted-foreground">补偿后计费（uplifted）</div>
                <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.reportedUsage)}</div>
                <div className="mt-1 font-medium">{formatUsd(billing.upliftedCostUsd ?? billing.reportedCostUsd ?? billing.billableCostUsd ?? 0)}</div>
              </div>
              <div>
                <div className="text-xs text-muted-foreground">上报费用 / 可计费</div>
                <div className="mt-1 font-medium">{formatUsd(billing.reportedCostUsd)} / {formatUsd(billing.billableCostUsd)}</div>
              </div>
              {billing.profitUsd !== undefined && (
                <div>
                  <div className="text-xs text-muted-foreground">净盈亏</div>
                  <div className={`mt-1 font-medium ${billing.profitUsd >= 0 ? 'text-success' : 'text-destructive'}`}>
                    {billing.profitUsd >= 0 ? '+' : ''}{formatUsd(billing.profitUsd)}
                  </div>
                </div>
              )}
              <div>
                <div className="text-xs text-muted-foreground">计价模型 / 用量模式</div>
                <div className="mt-0.5 text-xs">{billing.pricingAvailable ? billing.pricingModel || 'priced' : 'unpriced'}</div>
                <div className="text-xs text-muted-foreground">{billing.usageProjectionApplied ? '已按入口规则展示' : '保持原样'}</div>
              </div>
            </div>
          </div>
        )}

        {/* 错误详情 */}
        <div>
          <SectionTitle>错误详情</SectionTitle>
          <pre className="max-h-60 overflow-auto rounded-lg border border-border bg-muted/40 p-3 text-xs whitespace-pre-wrap break-words">
            {record.errorDetail || record.errorMessage || '-'}
          </pre>
        </div>

        {/* 请求内容诊断 */}
        {Boolean(record.payloadBreakdown || record.payloadGuardReport) && (
          <div>
            <SectionTitle>请求内容诊断</SectionTitle>
            <pre className="max-h-72 overflow-auto rounded-lg border border-border bg-muted/40 p-3 text-xs whitespace-pre-wrap break-words">
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
