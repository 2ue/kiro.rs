import type { ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { fmtCompact } from '@/lib/format'

/** 横向排行条：名称 + 数值 + 占比条 */
export function RankList({
  items,
  valueFormatter = fmtCompact,
  color = 'var(--chart-1)',
  footer,
  onSelect,
}: {
  items: Array<{ key: string; label: string; value: number; sub?: string }>
  valueFormatter?: (v: number) => string
  color?: string
  footer?: ReactNode
  onSelect?: (key: string) => void
}) {
  const max = Math.max(1, ...items.map((i) => i.value))
  return (
    <div className="space-y-2">
      <ul className="space-y-1.5">
        {items.map((i) => {
          const content = (
            <>
              <div className="flex items-baseline justify-between gap-2 text-xs">
                <span className="min-w-0 truncate" title={i.label}>
                  {i.label}
                </span>
                <span className="num shrink-0 font-medium">
                  {valueFormatter(i.value)}
                  {i.sub && <span className="ml-1.5 font-normal text-muted-foreground">{i.sub}</span>}
                </span>
              </div>
              <div className="mt-1 h-1 overflow-hidden rounded-full bg-muted">
                <div className="h-full rounded-full" style={{ width: `${(i.value / max) * 100}%`, background: color }} />
              </div>
            </>
          )
          return (
            <li key={i.key}>
              {onSelect ? (
                <Button
                  variant="unstyled"
                  size="none"
                  onClick={() => onSelect(i.key)}
                  className="block w-full rounded-sm text-left hover:opacity-80"
                >
                  {content}
                </Button>
              ) : (
                content
              )}
            </li>
          )
        })}
      </ul>
      {footer}
    </div>
  )
}
