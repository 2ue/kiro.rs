import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/** 详情面板中的键值网格 */
export function StatGrid({ children, className, cols = 2 }: { children: ReactNode; className?: string; cols?: 2 | 3 | 4 }) {
  return (
    <dl
      className={cn(
        'grid gap-x-4 gap-y-3',
        cols === 2 && 'grid-cols-2',
        cols === 3 && 'grid-cols-2 sm:grid-cols-3',
        cols === 4 && 'grid-cols-2 sm:grid-cols-4',
        className,
      )}
    >
      {children}
    </dl>
  )
}

export function Stat({ label, children, className, mono }: { label: ReactNode; children: ReactNode; className?: string; mono?: boolean }) {
  return (
    <div className={cn('min-w-0', className)}>
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className={cn('num mt-0.5 truncate text-sm', mono && 'font-mono text-xs')}>{children}</dd>
    </div>
  )
}
