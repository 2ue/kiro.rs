import type { ColumnDef } from '@tanstack/react-table'
import { CredentialStatusBadge } from '@/components/status/credential-status-badge'
import { QuotaBar } from '@/components/status/quota-bar'
import { ToneBadge } from '@/components/status/tone-badge'
import { selectionColumn } from '@/components/data-table/selection'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { authMethodLabel, PROXY_SOURCE_LABEL, SUBSCRIPTION_LABEL, subscriptionTier } from '@/domain/labels'
import { fmtCompact, fmtPct, fmtRelative, fmtUsd } from '@/lib/format'
import type { AccountRow } from './queries'
import type { Tone } from '@/components/status/tone'

const TIER_TONE: Record<string, Tone> = {
  power: 'primary',
  pro_max: 'primary',
  pro_plus: 'primary',
  pro: 'info',
  free: 'neutral',
  trial: 'warning',
  other: 'neutral',
  unknown: 'neutral',
}

export const ACCOUNT_COLUMN_LABELS: Array<{ id: string; label: string }> = [
  { id: 'subscription', label: '订阅' },
  { id: 'quota', label: '额度' },
  { id: 'load', label: '在途/并发' },
  { id: 'scheduler', label: '调度' },
  { id: 'errorRate', label: '错误率' },
  { id: 'cost', label: '估算费用' },
  { id: 'proxy', label: '代理' },
  { id: 'priority', label: '优先级' },
  { id: 'lastUsed', label: '最近使用' },
]

export function accountColumns(): ColumnDef<AccountRow, unknown>[] {
  return [
    selectionColumn<AccountRow>(),
    {
      id: 'account',
      header: '账号',
      size: 240,
      meta: { grow: true },
      cell: ({ row: { original: r } }) => (
        <div className="flex min-w-0 flex-col leading-tight">
          <span className="truncate font-medium">{r.label}</span>
          <span className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
            <span className="num">#{r.id}</span>
            <span>·</span>
            <span>{authMethodLabel(r.authMethod)}</span>
            {r.tags?.slice(0, 2).map((t) => (
              <span key={t} className="rounded bg-muted px-1">
                {t}
              </span>
            ))}
            {(r.tags?.length ?? 0) > 2 && <span>+{(r.tags?.length ?? 0) - 2}</span>}
          </span>
        </div>
      ),
    },
    {
      id: 'status',
      header: '状态',
      size: 140,
      cell: ({ row: { original: r } }) => <CredentialStatusBadge view={r.status} />,
    },
    {
      id: 'subscription',
      header: '订阅',
      size: 90,
      cell: ({ row: { original: r } }) => {
        const tier = subscriptionTier(r.accountInfo?.subscriptionTitle ?? r.subscriptionTitle)
        return (
          <ToneBadge tone={TIER_TONE[tier]} title={r.accountInfo?.subscriptionTitle ?? r.subscriptionTitle ?? undefined}>
            {SUBSCRIPTION_LABEL[tier]}
          </ToneBadge>
        )
      },
    },
    {
      id: 'quota',
      header: '额度',
      size: 170,
      cell: ({ row: { original: r } }) => (
        <div className="flex items-center gap-2">
          <QuotaBar quota={r.quota} className="flex-1" />
          {r.quota.nextResetAt && (
            <span className="num shrink-0 text-xs text-muted-foreground">{fmtRelative(r.quota.nextResetAt.getTime())}</span>
          )}
        </div>
      ),
    },
    {
      id: 'load',
      header: '在途/并发',
      size: 96,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => (
        <span className={r.inFlightRequests > 0 ? 'text-foreground' : 'text-muted-foreground'}>
          {r.inFlightRequests}
          <span className="text-muted-foreground">/{r.maxConcurrentRequests > 0 ? r.maxConcurrentRequests : '∞'}</span>
        </span>
      ),
    },
    {
      id: 'scheduler',
      header: '调度',
      size: 110,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => (
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="text-muted-foreground">
              <span className="text-foreground">{r.recentSchedulerSelectionCount5m ?? 0}</span> 次/5m
            </span>
          </TooltipTrigger>
          <TooltipContent className="num text-xs">
            评分 {(r.schedulerScore ?? 0).toFixed(3)}（越低越优先）· 选择压力 {(r.schedulerSelectionPressure ?? 0).toFixed(2)}
          </TooltipContent>
        </Tooltip>
      ),
    },
    {
      id: 'errorRate',
      header: '错误率',
      size: 80,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => {
        const rate = r.recentErrorRate ?? 0
        return <span className={rate >= 0.5 ? 'text-danger' : rate >= 0.1 ? 'text-warning' : 'text-muted-foreground'}>{fmtPct(rate, 0)}</span>
      },
    },
    {
      id: 'cost',
      header: '估算费用',
      size: 96,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => (
        <span title={`Kiro metering ${fmtCompact(r.kiroMeteringUsage)}`}>{fmtUsd(r.estimatedCostUsd)}</span>
      ),
    },
    {
      id: 'proxy',
      header: '代理',
      size: 120,
      cell: ({ row: { original: r } }) => {
        const blocked = r.effectiveProxySource === 'resource_disabled' || r.effectiveProxySource === 'resource_missing'
        return (
          <span className={blocked ? 'text-danger' : 'text-muted-foreground'} title={r.proxyResourceName ?? r.effectiveProxyUrl}>
            {r.proxyResourceName && r.effectiveProxySource.startsWith('resource') ? r.proxyResourceName : PROXY_SOURCE_LABEL[r.effectiveProxySource]}
          </span>
        )
      },
    },
    {
      id: 'priority',
      header: '优先级',
      size: 70,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => <span className={r.priority === 0 ? 'text-muted-foreground' : ''}>{r.priority}</span>,
    },
    {
      id: 'lastUsed',
      header: '最近使用',
      size: 96,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => <span className="text-muted-foreground">{r.lastUsedAt ? fmtRelative(r.lastUsedAt) : '从未'}</span>,
    },
  ]
}
