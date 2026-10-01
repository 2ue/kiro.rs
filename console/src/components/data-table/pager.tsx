import { ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { fmtInt } from '@/lib/format'

export function Pager({
  page,
  totalPages,
  total,
  pageSize,
  pageSizes = [20, 50, 100, 200],
  onPageChange,
  onPageSizeChange,
}: {
  page: number
  totalPages: number
  total?: number
  pageSize: number
  pageSizes?: number[]
  onPageChange: (page: number) => void
  onPageSizeChange?: (size: number) => void
}) {
  const last = Math.max(1, totalPages)
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 text-xs text-muted-foreground">
      <span className="num">
        {typeof total === 'number' ? `共 ${fmtInt(total)} 条 · ` : ''}第 {page} / {last} 页
      </span>
      <div className="flex items-center gap-2">
        {onPageSizeChange && (
          <Select value={String(pageSize)} onValueChange={(v) => onPageSizeChange(Number(v))}>
            <SelectTrigger size="sm" className="w-auto" aria-label="每页条数">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {pageSizes.map((s) => (
                <SelectItem key={s} value={String(s)}>
                  {s} 条/页
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        <div className="flex items-center">
          <Button variant="ghost" size="icon-sm" disabled={page <= 1} onClick={() => onPageChange(1)} aria-label="第一页">
            <ChevronsLeft />
          </Button>
          <Button variant="ghost" size="icon-sm" disabled={page <= 1} onClick={() => onPageChange(page - 1)} aria-label="上一页">
            <ChevronLeft />
          </Button>
          <Button variant="ghost" size="icon-sm" disabled={page >= last} onClick={() => onPageChange(page + 1)} aria-label="下一页">
            <ChevronRight />
          </Button>
          <Button variant="ghost" size="icon-sm" disabled={page >= last} onClick={() => onPageChange(last)} aria-label="最后一页">
            <ChevronsRight />
          </Button>
        </div>
      </div>
    </div>
  )
}
