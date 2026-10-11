import { useEffect, useMemo, useState, type FormEvent } from 'react'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui'
import { cn } from '@/lib/utils'

interface PaginationProps {
  page: number
  pageCount: number
  total?: number
  pageSize?: number
  pageSizeOptions?: number[]
  onPageChange: (page: number) => void
  onPageSizeChange?: (pageSize: number) => void
  pending?: boolean
  className?: string
}

type PageItem = number | 'ellipsis-left' | 'ellipsis-right'

function clampPage(page: number, pageCount: number) {
  return Math.min(Math.max(page, 1), Math.max(pageCount, 1))
}

function pageItems(page: number, pageCount: number): PageItem[] {
  if (pageCount <= 7) return Array.from({ length: pageCount }, (_, index) => index + 1)

  const pages = new Set<number>([1, pageCount, page, page - 1, page + 1])
  if (page <= 4) {
    for (let value = 2; value <= 5; value += 1) pages.add(value)
  }
  if (page >= pageCount - 3) {
    for (let value = pageCount - 4; value < pageCount; value += 1) pages.add(value)
  }

  const sorted = Array.from(pages)
    .filter((value) => value >= 1 && value <= pageCount)
    .sort((left, right) => left - right)

  return sorted.flatMap<PageItem>((value, index) => {
    const previous = sorted[index - 1]
    if (index === 0 || value === previous + 1) return [value]
    return [previous === 1 ? 'ellipsis-left' : 'ellipsis-right', value]
  })
}

/** 统一分页控件 */
export function Pagination({
  page,
  pageCount,
  total,
  pageSize,
  pageSizeOptions,
  onPageChange,
  onPageSizeChange,
  pending = false,
  className,
}: PaginationProps) {
  const normalizedPage = clampPage(page, pageCount)
  const [jumpValue, setJumpValue] = useState(String(normalizedPage))
  const pages = useMemo(() => pageItems(normalizedPage, Math.max(pageCount, 1)), [normalizedPage, pageCount])
  const pageSizes = useMemo(() => {
    const values = new Set(pageSizeOptions ?? [])
    if (pageSize) values.add(pageSize)
    return Array.from(values).filter(Boolean).sort((left, right) => left - right)
  }, [pageSize, pageSizeOptions])

  useEffect(() => {
    setJumpValue(String(normalizedPage))
  }, [normalizedPage])

  if (pageCount <= 0 && !total) return null

  const canPrev = normalizedPage > 1 && !pending
  const canNext = normalizedPage < pageCount && !pending

  const rangeStart = total && pageSize ? (normalizedPage - 1) * pageSize + 1 : undefined
  const rangeEnd = total && pageSize ? Math.min(normalizedPage * pageSize, total) : undefined

  const handleJump = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (pending) return
    const next = clampPage(Number(jumpValue), pageCount)
    if (Number.isFinite(next) && next !== normalizedPage) {
      onPageChange(next)
    } else {
      setJumpValue(String(normalizedPage))
    }
  }

  return (
    <div className={cn('flex flex-wrap items-center justify-between gap-3', className)}>
      <div className="min-w-0 text-xs text-muted-foreground">
        {total !== undefined ? (
          <span className="tabular-nums">
            {rangeStart}-{rangeEnd} / 共 {total} 条
            {pending ? ' · 加载中' : ''}
          </span>
        ) : (
          <span className="tabular-nums">
            第 {page} / {pageCount} 页
            {pending ? ' · 加载中' : ''}
          </span>
        )}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2">
        {pageSizes.length > 0 && onPageSizeChange && (
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <span className="whitespace-nowrap">每页</span>
            <Select
              value={String(pageSize)}
              onValueChange={(value) => onPageSizeChange(Number(value))}
              disabled={pending}
            >
              <SelectTrigger size="sm" className="h-8 w-[5.25rem]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {pageSizes.map((value) => (
                  <SelectItem key={value} value={String(value)}>
                    {value} 条
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
        <div className="flex items-center gap-1">
          <Button
            variant="outline"
            size="icon-sm"
            disabled={!canPrev}
            onClick={() => onPageChange(normalizedPage - 1)}
            aria-label="上一页"
          >
            <ChevronLeft className="size-4" />
          </Button>
          {pages.map((item) => item === 'ellipsis-left' || item === 'ellipsis-right' ? (
            <span
              key={item}
              className="flex size-8 items-center justify-center text-xs text-muted-foreground"
              aria-hidden="true"
            >
              ...
            </span>
          ) : (
            <Button
              key={item}
              variant={item === normalizedPage ? 'default' : 'outline'}
              size="icon-sm"
              disabled={pending}
              aria-current={item === normalizedPage ? 'page' : undefined}
              onClick={() => item !== normalizedPage && onPageChange(item)}
              className="tabular-nums"
            >
              {item}
            </Button>
          ))}
          <Button
            variant="outline"
            size="icon-sm"
            disabled={!canNext}
            onClick={() => onPageChange(normalizedPage + 1)}
            aria-label="下一页"
          >
            <ChevronRight className="size-4" />
          </Button>
        </div>
        <form className="flex items-center gap-1.5" onSubmit={handleJump}>
          <Input
            className="h-8 w-16 px-2 text-center text-xs tabular-nums"
            value={jumpValue}
            inputMode="numeric"
            pattern="[0-9]*"
            disabled={pending}
            aria-label="跳转页码"
            onChange={(event) => setJumpValue(event.target.value.replace(/\D/g, ''))}
          />
          <Button variant="outline" size="sm" disabled={pending || pageCount <= 1} type="submit">
            跳转
          </Button>
        </form>
      </div>
    </div>
  )
}
