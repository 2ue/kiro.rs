import * as React from 'react'
import { AlertCircle, Inbox } from 'lucide-react'
import { cn } from '@/lib/utils'
import { Spinner } from '@/components/ui'

/** 空状态 */
export function EmptyState({
  icon,
  title = '暂无数据',
  description,
  action,
  className,
}: {
  icon?: React.ReactNode
  title?: string
  description?: string
  action?: React.ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center rounded-xl border border-dashed border-border bg-muted/30 px-6 py-14 text-center',
        className
      )}
    >
      <div className="mb-3 text-muted-foreground/40 [&_svg]:size-10">{icon ?? <Inbox />}</div>
      <div className="text-sm font-semibold text-foreground/70">{title}</div>
      {description && <div className="mt-1 max-w-sm text-xs text-muted-foreground">{description}</div>}
      {action && <div className="mt-4">{action}</div>}
    </div>
  )
}

/** 加载状态 */
export function LoadingState({ text = '加载中...', className }: { text?: string; className?: string }) {
  return (
    <div className={cn('flex flex-col items-center justify-center gap-3 py-14', className)}>
      <Spinner size="md" />
      <span className="text-sm text-muted-foreground">{text}</span>
    </div>
  )
}

/** 错误状态 */
export function ErrorState({
  title = '加载失败',
  message = '发生未知错误',
  action,
  className,
}: {
  title?: string
  message?: string
  action?: React.ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        'flex flex-col items-start gap-2 rounded-xl border border-destructive/30 bg-destructive/5 px-4 py-3.5 text-sm',
        className
      )}
    >
      <div className="flex items-center gap-2 font-semibold text-destructive">
        <AlertCircle className="size-4" />
        {title}
      </div>
      <div className="text-destructive/80">{message}</div>
      {action && <div className="mt-1">{action}</div>}
    </div>
  )
}

/** 内联提示框 */
export function Callout({
  tone = 'info',
  children,
  className,
}: {
  tone?: 'info' | 'warning' | 'success' | 'error'
  children: React.ReactNode
  className?: string
}) {
  const toneClass = {
    info: 'border-info/30 bg-info/5 text-info',
    warning: 'border-warning/30 bg-warning/5 text-warning',
    success: 'border-success/30 bg-success/5 text-success',
    error: 'border-destructive/30 bg-destructive/5 text-destructive',
  }[tone]
  return (
    <div className={cn('rounded-lg border px-3.5 py-2.5 text-xs leading-relaxed', toneClass, className)}>
      {children}
    </div>
  )
}
