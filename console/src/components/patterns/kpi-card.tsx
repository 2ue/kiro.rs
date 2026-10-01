import { lazy, Suspense, type ReactNode } from 'react'
import { ArrowDownRight, ArrowUpRight } from 'lucide-react'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

// 图表库按需加载，避免进入首屏包
const Sparkline = lazy(() => import('@/components/charts/sparkline'))

export function KpiCard({
  label,
  value,
  hint,
  delta,
  /** 增长是否是好事（错误率、延迟增长为坏） */
  deltaGoodWhenUp = true,
  spark,
  sparkColor = 'var(--chart-1)',
  loading,
  className,
  icon,
}: {
  label: string
  value: ReactNode
  hint?: ReactNode
  delta?: number | null
  deltaGoodWhenUp?: boolean
  spark?: number[]
  sparkColor?: string
  loading?: boolean
  className?: string
  icon?: ReactNode
}) {
  const hasDelta = typeof delta === 'number' && Number.isFinite(delta)
  const up = hasDelta && delta > 0
  const good = hasDelta && (delta === 0 || up === deltaGoodWhenUp)
  return (
    <div className={cn('relative flex min-w-0 flex-col gap-1 overflow-hidden rounded-xl border bg-card p-4', className)}>
      <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5 truncate">
          {icon}
          {label}
        </span>
        {hasDelta && (
          <span className={cn('num inline-flex items-center gap-0.5 font-medium', good ? 'text-success' : 'text-danger')}>
            {up ? <ArrowUpRight className="size-3" /> : <ArrowDownRight className="size-3" />}
            {Math.abs(delta * 100).toFixed(0)}%
          </span>
        )}
      </div>
      {loading ? <Skeleton className="mt-1 h-7 w-24" /> : <div className="num truncate text-2xl font-semibold tracking-tight">{value}</div>}
      {hint && <div className="truncate text-xs text-muted-foreground">{hint}</div>}
      {spark && spark.length > 1 && (
        <div className="pointer-events-none mt-1 h-8" aria-hidden>
          <Suspense fallback={null}>
            <Sparkline values={spark} color={sparkColor} id={label} />
          </Suspense>
        </div>
      )}
    </div>
  )
}
