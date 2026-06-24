import { ChevronLeft, ChevronRight } from 'lucide-react'
import { Button } from '@/components/ui'
import { cn } from '@/lib/utils'

interface PaginationProps {
  page: number
  pageCount: number
  total?: number
  pageSize?: number
  onPageChange: (page: number) => void
  className?: string
}

/** 统一分页控件:取代各面板复制的 Math.max(1, page-1) 逻辑 */
export function Pagination({
  page,
  pageCount,
  total,
  pageSize,
  onPageChange,
  className,
}: PaginationProps) {
  if (pageCount <= 1 && !total) return null

  const canPrev = page > 1
  const canNext = page < pageCount

  const rangeStart = total && pageSize ? (page - 1) * pageSize + 1 : undefined
  const rangeEnd = total && pageSize ? Math.min(page * pageSize, total) : undefined

  return (
    <div className={cn('flex flex-wrap items-center justify-between gap-3', className)}>
      <div className="text-xs text-muted-foreground">
        {total !== undefined ? (
          <span className="tabular-nums">
            {rangeStart}-{rangeEnd} / 共 {total} 条
          </span>
        ) : (
          <span className="tabular-nums">
            第 {page} / {pageCount} 页
          </span>
        )}
      </div>
      <div className="flex items-center gap-1.5">
        <Button
          variant="outline"
          size="icon-sm"
          disabled={!canPrev}
          onClick={() => onPageChange(Math.max(1, page - 1))}
          aria-label="上一页"
        >
          <ChevronLeft className="size-4" />
        </Button>
        <span className="min-w-[5rem] text-center text-xs font-medium tabular-nums text-foreground/80">
          {page} / {pageCount}
        </span>
        <Button
          variant="outline"
          size="icon-sm"
          disabled={!canNext}
          onClick={() => onPageChange(Math.min(pageCount, page + 1))}
          aria-label="下一页"
        >
          <ChevronRight className="size-4" />
        </Button>
      </div>
    </div>
  )
}
