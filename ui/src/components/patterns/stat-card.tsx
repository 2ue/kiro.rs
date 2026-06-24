import * as React from 'react'
import { cn } from '@/lib/utils'

/** 统计指标卡片 */
interface StatCardProps {
  title: string
  value: React.ReactNode
  desc?: React.ReactNode
  icon?: React.ReactNode
  tone?: 'default' | 'primary' | 'success' | 'warning' | 'error' | 'info'
  className?: string
}

const accentMap: Record<string, string> = {
  default: 'bg-secondary',
  primary: 'bg-primary',
  success: 'bg-success',
  warning: 'bg-warning',
  error: 'bg-destructive',
  info: 'bg-info',
}

const iconMap: Record<string, string> = {
  default: 'text-muted-foreground',
  primary: 'text-primary',
  success: 'text-success',
  warning: 'text-warning',
  error: 'text-destructive',
  info: 'text-info',
}

const valueMap: Record<string, string> = {
  default: 'text-foreground',
  primary: 'text-primary',
  success: 'text-success',
  warning: 'text-warning',
  error: 'text-destructive',
  info: 'text-info',
}

export function StatCard({ title, value, desc, icon, tone = 'default', className }: StatCardProps) {
  return (
    <div
      className={cn(
        'relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl border border-border bg-card p-4 shadow-sm transition-colors hover:border-border-strong',
        className
      )}
    >
      <span className={cn('absolute left-0 top-4 h-8 w-1 rounded-r-full', accentMap[tone])} />
      <div className="flex items-start justify-between gap-2 pl-2.5">
        <div className="min-w-0 flex-1">
          <div className="text-[0.72rem] font-semibold text-muted-foreground">{title}</div>
          <div className={cn('mt-1 break-words text-2xl font-semibold tracking-tight tabular-nums', valueMap[tone])}>
            {value}
          </div>
        </div>
        {icon && (
          <div className={cn('flex size-9 shrink-0 items-center justify-center rounded-lg border border-border bg-muted [&_svg]:size-4', iconMap[tone])}>
            {icon}
          </div>
        )}
      </div>
      {desc && <div className="mt-2 truncate pl-2.5 text-[0.72rem] text-muted-foreground">{desc}</div>}
    </div>
  )
}

/** 统计卡网格 */
export function StatGrid({
  children,
  className,
  min = '11rem',
}: {
  children: React.ReactNode
  className?: string
  min?: string
}) {
  return (
    <div
      className={cn('grid gap-3', className)}
      style={{ gridTemplateColumns: `repeat(auto-fill, minmax(min(${min}, 100%), 1fr))` }}
    >
      {children}
    </div>
  )
}
