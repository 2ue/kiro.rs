import * as React from 'react'
import { cn } from '@/lib/utils'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '@/components/ui'

/** 区块卡片:统一的内容容器(标题 + 操作 + 内容) */
interface SectionCardProps {
  title?: React.ReactNode
  description?: React.ReactNode
  actions?: React.ReactNode
  children: React.ReactNode
  className?: string
  bodyClassName?: string
  noPadding?: boolean
  icon?: React.ReactNode
}

export function SectionCard({
  title,
  description,
  actions,
  children,
  className,
  bodyClassName,
  noPadding,
  icon,
}: SectionCardProps) {
  const hasHeader = title || description || actions
  return (
    <Card className={cn('overflow-hidden', className)}>
      {hasHeader && (
        <CardHeader>
          <div className="flex min-w-0 items-start gap-3">
            {icon && (
              <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary [&_svg]:size-4">
                {icon}
              </span>
            )}
            <div className="min-w-0">
              {title && <CardTitle>{title}</CardTitle>}
              {description && <CardDescription>{description}</CardDescription>}
            </div>
          </div>
          {actions && <div className="flex shrink-0 flex-wrap items-center gap-1.5">{actions}</div>}
        </CardHeader>
      )}
      {noPadding ? <div className={bodyClassName}>{children}</div> : <CardContent className={bodyClassName}>{children}</CardContent>}
    </Card>
  )
}
