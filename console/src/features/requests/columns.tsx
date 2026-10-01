import type { ColumnDef } from '@tanstack/react-table'
import type { UsageRecord } from '@/api/types'
import { ToneBadge } from '@/components/status/tone-badge'
import type { Tone } from '@/components/status/tone'
import { RECORD_STATUS_LABEL, routeLabel } from '@/domain/labels'
import { fmtCompact, fmtDateTime, fmtMs, fmtUsd } from '@/lib/format'

export function recordTone(status: string): Tone {
  if (status === 'success') return 'success'
  if (status === 'client_dropped') return 'warning'
  return 'danger'
}

export const REQUEST_COLUMN_LABELS = [
  { id: 'route', label: '路由' },
  { id: 'target', label: '账号/外部池' },
  { id: 'tokens', label: 'Tokens' },
  { id: 'cache', label: '缓存读' },
  { id: 'cost', label: '费用' },
  { id: 'ttft', label: '首字' },
  { id: 'duration', label: '耗时' },
  { id: 'endpoint', label: '端点' },
  { id: 'key', label: '请求 Key' },
]

export function requestColumns(): ColumnDef<UsageRecord, unknown>[] {
  return [
    {
      id: 'time',
      header: '时间',
      size: 128,
      meta: { mobile: true },
      cell: ({ row: { original: r } }) => <span className="num text-muted-foreground">{fmtDateTime(r.createdAt, true)}</span>,
    },
    {
      id: 'status',
      header: '状态',
      size: 96,
      meta: { mobile: 'badge' },
      cell: ({ row: { original: r } }) => (
        <ToneBadge tone={recordTone(r.status)} dot>
          {RECORD_STATUS_LABEL[r.status] ?? r.status}
          {r.errorStatusCode ? <span className="num opacity-80">{r.errorStatusCode}</span> : null}
        </ToneBadge>
      ),
    },
    {
      id: 'model',
      header: '模型',
      size: 200,
      meta: { mobile: 'title', grow: true },
      cell: ({ row: { original: r } }) => (
        <div className="flex min-w-0 items-center gap-1.5">
          <span className="truncate font-mono text-xs">{r.model}</span>
          {r.stream && <span className="rounded bg-muted px-1 text-2xs text-muted-foreground">SSE</span>}
          {r.credentialAttempts && r.credentialAttempts.length > 1 && (
            <span className="rounded bg-warning-subtle px-1 text-2xs text-warning">×{r.credentialAttempts.length}</span>
          )}
        </div>
      ),
    },
    {
      id: 'route',
      header: '路由',
      size: 132,
      meta: { mobile: true },
      cell: ({ row: { original: r } }) => (
        <span className={r.routeKind === 'external_pool' ? 'text-info' : 'text-muted-foreground'}>{routeLabel(r)}</span>
      ),
    },
    {
      id: 'target',
      header: '账号/外部池',
      size: 168,
      meta: { mobile: true },
      cell: ({ row: { original: r } }) => (
        <span className="truncate text-muted-foreground">
          {r.routeKind === 'external_pool'
            ? (r.externalPoolName ?? `池 #${r.externalPoolId}`)
            : (r.credentialLabel ?? (r.credentialId ? `#${r.credentialId}` : '—'))}
        </span>
      ),
    },
    {
      id: 'tokens',
      header: '输入→输出',
      size: 110,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: r } }) => (
        <span>
          {fmtCompact(r.totalInputTokens)}
          <span className="text-muted-foreground">→</span>
          {fmtCompact(r.outputTokens)}
        </span>
      ),
    },
    {
      id: 'cache',
      header: '缓存读',
      size: 80,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => (
        <span className={r.cacheReadInputTokens > 0 ? '' : 'text-muted-foreground'}>{fmtCompact(r.cacheReadInputTokens)}</span>
      ),
    },
    {
      id: 'cost',
      header: '费用',
      size: 84,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: r } }) => (
        <span className={r.pricingAvailable ? '' : 'text-muted-foreground'} title={r.pricingAvailable ? undefined : '无价格数据'}>
          {fmtUsd(r.estimatedCostUsd)}
        </span>
      ),
    },
    {
      id: 'ttft',
      header: '首字',
      size: 72,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => <span className="text-muted-foreground">{fmtMs(r.firstTokenLatencyMs)}</span>,
    },
    {
      id: 'duration',
      header: '耗时',
      size: 72,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: r } }) => <span>{fmtMs(r.durationMs)}</span>,
    },
    {
      id: 'endpoint',
      header: '端点',
      size: 140,
      cell: ({ row: { original: r } }) => <span className="truncate font-mono text-xs text-muted-foreground">{r.endpoint}</span>,
    },
    {
      id: 'key',
      header: '请求 Key',
      size: 110,
      cell: ({ row: { original: r } }) => (
        <span className="truncate font-mono text-xs text-muted-foreground">{r.requestApiKeyId ?? '—'}</span>
      ),
    },
  ]
}
