import { CredentialStatusBadge } from '@/components/status/credential-status-badge'
import { Button } from '@/components/ui/button'
import { QuotaBar } from '@/components/status/quota-bar'
import { Skeleton } from '@/components/ui/skeleton'
import { EmptyState } from '@/components/patterns/data-state'
import { authMethodLabel, SUBSCRIPTION_LABEL, subscriptionTier } from '@/domain/labels'
import { fmtRelative, fmtUsd } from '@/lib/format'
import { cn } from '@/lib/utils'
import type { AccountRow } from './queries'

/** 卡片视图：适合小屏和少量账号的快速巡检 */
export function AccountCards({
  rows,
  loading,
  onOpen,
  activeId,
}: {
  rows: AccountRow[]
  loading: boolean
  onOpen: (r: AccountRow) => void
  activeId?: number
}) {
  if (loading) {
    return (
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4">
        {Array.from({ length: 8 }, (_, i) => (
          <Skeleton key={i} className="h-36 rounded-xl" />
        ))}
      </div>
    )
  }
  if (rows.length === 0) return <EmptyState title="没有匹配的账号" />
  return (
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4">
      {rows.map((r) => (
        <Button
          variant="unstyled"
          size="none"
          key={r.id}
          type="button"
          onClick={() => onOpen(r)}
          className={cn(
            'flex flex-col gap-3 rounded-xl border bg-card p-4 text-left transition-colors hover:border-primary/40 focus-visible:outline-2 focus-visible:outline-ring',
            activeId === r.id && 'border-primary/60 ring-1 ring-primary/30',
            r.disabled && 'opacity-70',
          )}
        >
          <div className="flex items-start justify-between gap-2">
            <div className="min-w-0">
              <div className="truncate text-sm font-medium">{r.label}</div>
              <div className="num text-xs text-muted-foreground">
                #{r.id} · {authMethodLabel(r.authMethod)} ·{' '}
                {SUBSCRIPTION_LABEL[subscriptionTier(r.accountInfo?.subscriptionTitle ?? r.subscriptionTitle)]}
              </div>
            </div>
            <CredentialStatusBadge view={r.status} compact />
          </div>
          <QuotaBar quota={r.quota} />
          <div className="num grid grid-cols-3 gap-2 text-xs">
            <div>
              <div className="text-muted-foreground">在途</div>
              {r.inFlightRequests}/{r.maxConcurrentRequests || '∞'}
            </div>
            <div>
              <div className="text-muted-foreground">费用</div>
              {fmtUsd(r.estimatedCostUsd)}
            </div>
            <div>
              <div className="text-muted-foreground">重置</div>
              {r.quota.nextResetAt ? fmtRelative(r.quota.nextResetAt.getTime()) : '—'}
            </div>
          </div>
        </Button>
      ))}
    </div>
  )
}
