import * as React from 'react'
import { cn } from '@/lib/utils'

/** 页面头部:标题 + 副标题 + 操作区。用于每个 feature 页面顶部 */
export function PageHeader({
  title,
  subtitle,
  actions,
  className,
}: {
  title: React.ReactNode
  subtitle?: React.ReactNode
  actions?: React.ReactNode
  className?: string
}) {
  return (
    <div className={cn('flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between', className)}>
      <div className="min-w-0">
        <h1 className="flex items-center gap-2.5 text-xl font-semibold tracking-tight text-foreground">
          <span className="h-6 w-1 rounded-full bg-primary" />
          <span className="truncate">{title}</span>
        </h1>
        {subtitle && <p className="mt-1 truncate text-sm text-muted-foreground">{subtitle}</p>}
      </div>
      {actions && <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>}
    </div>
  )
}

/** 页面容器:统一最大宽度与纵向间距 */
export function PageContainer({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return <div className={cn('mx-auto flex w-full max-w-[1600px] flex-col gap-5', className)}>{children}</div>
}
