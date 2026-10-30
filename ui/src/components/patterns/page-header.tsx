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

/** 页面容器:统一纵向间距;宽度随窗口铺满,宽屏下不留两侧空白 */
export function PageContainer({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return <div className={cn('flex w-full flex-col gap-4', className)}>{children}</div>
}
