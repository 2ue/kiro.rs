import { Ban, CirclePlay, Eraser, MoreHorizontal, Pencil, ShieldCheck, Trash2 } from 'lucide-react'
import type { ExternalPoolStatus, UsageExternalPoolBillingByPool } from '@/api/types'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Meter } from '@/components/status/meter'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtInt, fmtMs, fmtPct, fmtUsd } from '@/lib/format'
import { cn } from '@/lib/utils'
import { poolState, usePoolActions } from './queries'

export function PoolCard({
  status,
  billing,
  active,
  onOpen,
  onEdit,
}: {
  status: ExternalPoolStatus
  billing?: UsageExternalPoolBillingByPool
  active?: boolean
  onOpen: () => void
  onEdit: () => void
}) {
  const p = status.pool
  const st = poolState(status)
  const q = status.quality
  const actions = usePoolActions()
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(e) => e.key === 'Enter' && e.target === e.currentTarget && onOpen()}
      className={cn(
        'flex cursor-pointer flex-col gap-3 rounded-xl border bg-card p-4 transition-colors hover:border-primary/40 focus-visible:outline-2 focus-visible:outline-ring',
        active && 'border-primary/60 ring-1 ring-primary/30',
        !p.enabled && 'opacity-70',
      )}
    >
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{p.name}</span>
            <span className="num rounded bg-muted px-1 text-xs text-muted-foreground">P{p.priority}</span>
          </div>
          <div className="truncate font-mono text-xs text-muted-foreground">{p.baseUrl}</div>
        </div>
        <div className="flex items-center gap-1" onClick={(e) => e.stopPropagation()}>
          <ToneBadge tone={st.tone} dot>
            {st.label}
          </ToneBadge>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon-xs" aria-label="更多操作">
                <MoreHorizontal />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={onEdit}>
                <Pencil /> 编辑
              </DropdownMenuItem>
              {p.enabled ? (
                <DropdownMenuItem onSelect={() => actions.toggleEnabled(p.id, p.name, false)}>
                  <Ban /> 停用
                </DropdownMenuItem>
              ) : (
                <DropdownMenuItem onSelect={() => actions.toggleEnabled(p.id, p.name, true)}>
                  <CirclePlay /> 启用
                </DropdownMenuItem>
              )}
              {status.cooldownRemainingSecs > 0 && (
                <DropdownMenuItem onSelect={() => actions.clearCooldown.mutate(p.id)}>
                  <Eraser /> 清除冷却
                </DropdownMenuItem>
              )}
              {p.autoDisabled && (
                <DropdownMenuItem onSelect={() => actions.clearAutoDisabled.mutate(p.id)}>
                  <ShieldCheck /> 解除自动禁用
                </DropdownMenuItem>
              )}
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onSelect={() => actions.confirmRemove(p.id, p.name)}>
                <Trash2 /> 删除
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
      {st.reason && <p className="line-clamp-2 text-xs text-muted-foreground">{st.reason}</p>}
      <div className="space-y-1">
        <div className="flex justify-between text-xs">
          <span className="text-muted-foreground">在途 / 并发</span>
          <span className="num">
            {status.inFlight} / {p.maxConcurrentRequests > 0 ? p.maxConcurrentRequests : '不限'}
          </span>
        </div>
        {p.maxConcurrentRequests > 0 && <Meter value={status.inFlight} max={p.maxConcurrentRequests} tone="info" />}
      </div>
      <div className="num grid grid-cols-4 gap-2 text-xs">
        <div>
          <div className="text-muted-foreground">错误率</div>
          <span className={(q?.recentErrorRate ?? 0) > 0.2 ? 'text-danger' : ''}>{q ? fmtPct(q.recentErrorRate, 0) : '—'}</span>
        </div>
        <div>
          <div className="text-muted-foreground">首字</div>
          {fmtMs(q?.ttftEwmaMs)}
        </div>
        <div>
          <div className="text-muted-foreground">请求</div>
          {billing ? fmtInt(billing.requests) : '—'}
        </div>
        <div>
          <div className="text-muted-foreground">利润</div>
          {billing?.profitUsd !== undefined ? (
            <span className={billing.profitUsd < 0 ? 'text-danger' : 'text-success'}>{fmtUsd(billing.profitUsd)}</span>
          ) : (
            '—'
          )}
        </div>
      </div>
      {q?.inProbation && (
        <div className="space-y-1">
          <div className="flex justify-between text-xs text-muted-foreground">
            <span>恢复进度</span>
            <span className="num">{fmtPct(q.recoveryProgress, 0)}</span>
          </div>
          <Meter value={q.recoveryProgress} max={1} tone="info" />
        </div>
      )}
    </div>
  )
}
