import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { formatNumber } from '@/lib/utils'

interface PaginationProps {
  /** 当前页(从 1 开始) */
  page: number
  /** 总页数 */
  totalPages: number
  /** 总条数 */
  totalItems?: number
  /** 当前每页大小 */
  pageSize: number
  /** 可选的每页大小列表 */
  pageSizeOptions: number[]
  /** 切换页码 */
  onPageChange: (page: number) => void
  /** 切换页大小(自动重置到第 1 页) */
  onPageSizeChange: (size: number) => void
  /** 是否显示总条数(默认 true) */
  showTotal?: boolean
}

/**
 * 通用分页组件 — 把"页码导航 + 每页大小切换 + 总数显示"集中在一起,
 * 所有需要分页的页面统一使用,避免散在多处。
 */
export function Pagination({
  page,
  totalPages,
  totalItems,
  pageSize,
  pageSizeOptions,
  onPageChange,
  onPageSizeChange,
  showTotal = true,
}: PaginationProps) {
  const safeTotalPages = Math.max(totalPages, 1)
  const canPrev = page > 1
  const canNext = page < safeTotalPages

  return (
    <div className="flex flex-wrap items-center justify-between gap-3 text-sm text-muted-foreground">
      <div className="flex items-center gap-3">
        {showTotal && typeof totalItems === 'number' && (
          <span>
            共 <span className="font-medium text-foreground tabular-nums">{formatNumber(totalItems)}</span> 条
          </span>
        )}
        <Select
          value={String(pageSize)}
          onValueChange={(v) => onPageSizeChange(Number(v))}
        >
          <SelectTrigger className="h-8 w-28">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {pageSizeOptions.map((n) => (
              <SelectItem key={n} value={String(n)}>
                {n} 条 / 页
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="flex items-center gap-3">
        <span className="tabular-nums">
          第 {page} / {safeTotalPages} 页
        </span>
        <div className="flex gap-1">
          <Button
            variant="outline"
            size="sm"
            onClick={() => onPageChange(1)}
            disabled={!canPrev}
          >
            首页
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => onPageChange(Math.max(1, page - 1))}
            disabled={!canPrev}
          >
            上一页
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => onPageChange(Math.min(safeTotalPages, page + 1))}
            disabled={!canNext}
          >
            下一页
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => onPageChange(safeTotalPages)}
            disabled={!canNext}
          >
            末页
          </Button>
        </div>
      </div>
    </div>
  )
}
