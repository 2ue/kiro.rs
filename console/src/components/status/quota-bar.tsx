import { Clock } from 'lucide-react'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import type { QuotaLevel, QuotaView } from '@/domain/quota'
import { fmtDateTime, fmtInt, fmtPct, fmtRelative, fmtUsd } from '@/lib/format'
import { cn } from '@/lib/utils'

const LEVEL_BAR: Record<QuotaLevel, string> = {
  ok: 'bg-success',
  warn: 'bg-warning',
  high: 'bg-orange',
  over: 'bg-danger',
  unknown: 'bg-muted-foreground/30',
}

/** 分色额度条：<50% 绿 / 50-80% 黄 / 80-100% 橙 / 超额红色斜纹 */
export function QuotaBar({ quota, className, showText = true }: { quota: QuotaView; className?: string; showText?: boolean }) {
  if (quota.level === 'unknown') {
    return (
      <div className={cn('flex min-w-0 items-center gap-2 text-xs text-muted-foreground', className)}>
        <div className="h-1.5 w-full max-w-24 rounded-full border border-dashed border-muted-foreground/40" />
        {showText && <span>未查询</span>}
      </div>
    )
  }
  const pct = Math.min(1, quota.ratio) * 100
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div className={cn('flex min-w-0 items-center gap-2', quota.stale && 'opacity-60', className)}>
          <div className="relative h-1.5 w-full max-w-24 min-w-12 overflow-hidden rounded-full bg-muted">
            <div className={cn('h-full rounded-full transition-[width]', LEVEL_BAR[quota.level])} style={{ width: `${pct}%` }} />
            {quota.level === 'over' && (
              <div className="absolute inset-0 bg-[repeating-linear-gradient(45deg,transparent_0_3px,rgb(255_255_255/0.35)_3px_6px)]" />
            )}
          </div>
          {showText && (
            <span className="num shrink-0 text-xs text-muted-foreground">
              {fmtPct(quota.ratio, 0)}
              {quota.stale && <Clock className="ml-1 inline size-3 align-[-2px]" aria-label="快照可能过期" />}
            </span>
          )}
        </div>
      </TooltipTrigger>
      <TooltipContent className="text-xs">
        <div className="num space-y-0.5">
          <div>
            已用 {fmtInt(quota.used)} / {fmtInt(quota.limit)}（剩余 {fmtInt(quota.remaining)}）
          </div>
          {quota.nextResetAt && (
            <div>
              重置于 {fmtDateTime(quota.nextResetAt.getTime())}（{fmtRelative(quota.nextResetAt.getTime())}）
            </div>
          )}
          {quota.overageEnabled && <div>超额已开启{quota.overageUsd > 0 ? `，已产生 ${fmtUsd(quota.overageUsd)}` : ''}</div>}
          <div className="opacity-70">
            快照 {quota.checkedAt ? fmtRelative(quota.checkedAt.getTime()) : '—'}
            {quota.stale ? '（可能过期）' : ''}
          </div>
        </div>
      </TooltipContent>
    </Tooltip>
  )
}
