import { Activity, CircleCheck, CircleSlash, Eye, Gauge, Layers, Sparkles, Timer, TriangleAlert, type LucideIcon } from 'lucide-react'
import { HoverCard, HoverCardContent, HoverCardTrigger } from '@/components/ui/hover-card'
import type { CredentialStatusView, PrimaryStatus } from '@/domain/credential-status'
import { fmtDuration } from '@/lib/format'
import { ToneBadge } from './tone-badge'
import type { Tone } from './tone'

export const STATUS_META: Record<PrimaryStatus, { label: string; tone: Tone; icon: LucideIcon }> = {
  healthy: { label: '可调度', tone: 'success', icon: CircleCheck },
  busy: { label: '在用', tone: 'info', icon: Activity },
  warmup: { label: '预热中', tone: 'primary', icon: Sparkles },
  probation: { label: '观察期', tone: 'info', icon: Eye },
  failing: { label: '异常', tone: 'danger', icon: TriangleAlert },
  saturated: { label: '并发满', tone: 'warning', icon: Layers },
  cooling: { label: '冷却中', tone: 'warning', icon: Timer },
  rateLimited: { label: '限流', tone: 'orange', icon: Gauge },
  disabled: { label: '已禁用', tone: 'neutral', icon: CircleSlash },
}

export function CredentialStatusBadge({ view, compact }: { view: CredentialStatusView; compact?: boolean }) {
  const meta = STATUS_META[view.primary]
  const Icon = meta.icon
  const label = view.primary === 'disabled' && view.disabledReason ? view.disabledReason.label : meta.label
  const suffix = view.remainingSecs && view.remainingSecs > 0 ? fmtDuration(view.remainingSecs) : null

  return (
    <HoverCard openDelay={250} closeDelay={80}>
      <HoverCardTrigger asChild>
        <span className="inline-flex cursor-default" tabIndex={0}>
          <ToneBadge tone={meta.tone} icon={<Icon aria-hidden />}>
            {label}
            {!compact && suffix && <span className="num opacity-80">· {suffix}</span>}
          </ToneBadge>
        </span>
      </HoverCardTrigger>
      <HoverCardContent className="w-72 p-3 text-xs" align="start">
        <div className="flex items-center gap-2 text-sm font-medium">
          <Icon className="size-4" aria-hidden />
          {meta.label}
          {view.primary === 'disabled' && view.disabledReason && (
            <span className="text-muted-foreground">· {view.disabledReason.label}</span>
          )}
        </div>
        {view.detail && <p className="mt-1.5 leading-relaxed text-muted-foreground">{view.detail}</p>}
        {suffix && <p className="mt-1 text-muted-foreground">预计 {suffix} 后恢复</p>}
        {view.disabledReason && (
          <p className="mt-1.5">
            建议：<span className="font-medium">{view.disabledReason.actionLabel}</span>
          </p>
        )}
        {view.secondary.length > 0 && (
          <div className="mt-2 flex flex-wrap gap-1 border-t pt-2">
            {view.secondary.map((s) => (
              <ToneBadge key={s} tone={STATUS_META[s].tone}>
                {STATUS_META[s].label}
              </ToneBadge>
            ))}
          </div>
        )}
        <p className="mt-2 text-muted-foreground">{view.schedulable ? '当前可被调度选中' : '当前不会被调度选中'}</p>
      </HoverCardContent>
    </HoverCard>
  )
}
