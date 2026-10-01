import type { ReactNode } from 'react'
import { CircleAlert, Inbox, RotateCw } from 'lucide-react'
import { errorMessage } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

export function EmptyState({
  title = '暂无数据',
  description,
  icon,
  action,
  className,
}: {
  title?: string
  description?: ReactNode
  icon?: ReactNode
  action?: ReactNode
  className?: string
}) {
  return (
    <Empty className={cn('py-10', className)}>
      <EmptyHeader>
        <EmptyMedia variant="icon">{icon ?? <Inbox />}</EmptyMedia>
        <EmptyTitle>{title}</EmptyTitle>
        {description && <EmptyDescription>{description}</EmptyDescription>}
      </EmptyHeader>
      {action && <EmptyContent>{action}</EmptyContent>}
    </Empty>
  )
}

export function ErrorState({ error, onRetry, className }: { error: unknown; onRetry?: () => void; className?: string }) {
  return (
    <Empty className={cn('py-10', className)}>
      <EmptyHeader>
        <EmptyMedia variant="icon" className="text-danger">
          <CircleAlert />
        </EmptyMedia>
        <EmptyTitle>加载失败</EmptyTitle>
        <EmptyDescription>{errorMessage(error)}</EmptyDescription>
      </EmptyHeader>
      {onRetry && (
        <EmptyContent>
          <Button variant="outline" size="sm" onClick={onRetry}>
            <RotateCw /> 重试
          </Button>
        </EmptyContent>
      )}
    </Empty>
  )
}

export function LoadingRows({ rows = 6, className }: { rows?: number; className?: string }) {
  return (
    <div className={cn('space-y-2', className)} aria-busy>
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className="h-8 w-full" />
      ))}
    </div>
  )
}

/** 统一处理 query 的 加载/错误/空 三态 */
export function QueryState<T>({
  query,
  children,
  empty,
  isEmpty,
  loading,
}: {
  query: { data: T | undefined; isLoading: boolean; error: unknown; refetch: () => unknown }
  children: (data: T) => ReactNode
  empty?: ReactNode
  isEmpty?: (data: T) => boolean
  loading?: ReactNode
}) {
  if (query.isLoading) return <>{loading ?? <LoadingRows />}</>
  if (query.error && query.data === undefined) return <ErrorState error={query.error} onRetry={() => query.refetch()} />
  if (query.data === undefined) return <>{loading ?? <LoadingRows />}</>
  if (isEmpty?.(query.data)) return <>{empty ?? <EmptyState />}</>
  return <>{children(query.data)}</>
}
