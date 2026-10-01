import { cn } from '@/lib/utils'
import { TONE_DOT, type Tone } from './tone'

/** 细进度条，用于容量水位、并发占用等 */
export function Meter({
  value,
  max,
  tone = 'primary',
  className,
  label,
}: {
  value: number
  max: number
  tone?: Tone
  className?: string
  label?: string
}) {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0
  return (
    <div
      className={cn('h-1.5 w-full overflow-hidden rounded-full bg-muted', className)}
      role="meter"
      aria-valuemin={0}
      aria-valuemax={max}
      aria-valuenow={value}
      aria-label={label}
    >
      <div className={cn('h-full rounded-full transition-[width]', TONE_DOT[tone])} style={{ width: `${pct}%` }} />
    </div>
  )
}

export interface SegmentItem {
  key: string
  label: string
  value: number
  tone: Tone
}

/** 分段条（Tracker 风格），用于账号池状态分布 */
export function SegmentBar({ items, className, onSelect }: { items: SegmentItem[]; className?: string; onSelect?: (key: string) => void }) {
  const total = items.reduce((sum, item) => sum + item.value, 0)
  return (
    <div className={cn('space-y-2', className)}>
      <div className="flex h-2.5 w-full gap-0.5 overflow-hidden rounded-full bg-muted">
        {total > 0 &&
          items
            .filter((item) => item.value > 0)
            .map((item) => (
              <div
                key={item.key}
                className={cn('h-full first:rounded-l-full last:rounded-r-full', TONE_DOT[item.tone])}
                style={{ width: `${(item.value / total) * 100}%` }}
                title={`${item.label} ${item.value}`}
              />
            ))}
      </div>
      <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs">
        {items.map((item) =>
          onSelect ? (
            <button
              key={item.key}
              type="button"
              onClick={() => onSelect(item.key)}
              className="inline-flex items-center gap-1.5 rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline-2"
            >
              <span className={cn('size-2 rounded-full', TONE_DOT[item.tone])} aria-hidden />
              {item.label}
              <span className="num font-medium text-foreground">{item.value}</span>
            </button>
          ) : (
            <span key={item.key} className="inline-flex items-center gap-1.5 text-muted-foreground">
              <span className={cn('size-2 rounded-full', TONE_DOT[item.tone])} aria-hidden />
              {item.label}
              <span className="num font-medium text-foreground">{item.value}</span>
            </span>
          ),
        )}
      </div>
    </div>
  )
}
