import * as React from 'react'
import { cn } from '@/lib/utils'

/**
 * 页面头部操作区。标题/副标题由顶部 Topbar 统一展示(见 layouts/topbar.tsx),
 * 此处不再重复渲染标题,仅保留页面级操作按钮。无操作按钮时不占用空间。
 */
export function PageHeader({
  title: _title,
  subtitle: _subtitle,
  actions,
  className,
}: {
  title?: React.ReactNode
  subtitle?: React.ReactNode
  actions?: React.ReactNode
  className?: string
}) {
  if (!actions) return null
  return (
    <div className={cn('flex flex-wrap items-center justify-end gap-2', className)}>
      {actions}
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
