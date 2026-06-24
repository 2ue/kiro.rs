import * as React from 'react'
import { cn } from '@/lib/utils'
import { Label } from '@/components/ui'

/** 表单字段:label + 控件 + 描述,统一表单字段排版 */
interface FieldProps {
  label?: React.ReactNode
  htmlFor?: string
  required?: boolean
  description?: React.ReactNode
  error?: React.ReactNode
  children: React.ReactNode
  className?: string
  /** 横排:label 在左,控件在右(用于设置项) */
  inline?: boolean
}

export function Field({
  label,
  htmlFor,
  required,
  description,
  error,
  children,
  className,
  inline,
}: FieldProps) {
  if (inline) {
    return (
      <div
        className={cn(
          'flex flex-col gap-2 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4',
          className
        )}
      >
        <div className="min-w-0">
          {label && (
            <Label htmlFor={htmlFor} className="text-[0.82rem]">
              {label}
              {required && <span className="ml-0.5 text-destructive">*</span>}
            </Label>
          )}
          {description && (
            <p className="mt-0.5 text-[0.7rem] leading-4 text-muted-foreground">{description}</p>
          )}
        </div>
        <div className="shrink-0">{children}</div>
        {error && <p className="text-[0.7rem] text-destructive">{error}</p>}
      </div>
    )
  }

  return (
    <div className={cn('flex min-w-0 flex-col gap-1.5', className)}>
      {label && (
        <Label htmlFor={htmlFor} className="flex items-center gap-1">
          {label}
          {required && <span className="text-destructive">*</span>}
        </Label>
      )}
      {children}
      {description && <p className="text-[0.7rem] leading-4 text-muted-foreground">{description}</p>}
      {error && <p className="text-[0.7rem] text-destructive">{error}</p>}
    </div>
  )
}

/** 表单网格:自适应多列 */
export function FieldGrid({
  children,
  className,
  min = '15rem',
}: {
  children: React.ReactNode
  className?: string
  min?: string
}) {
  return (
    <div
      className={cn('grid gap-4', className)}
      style={{ gridTemplateColumns: `repeat(auto-fit, minmax(min(${min}, 100%), 1fr))` }}
    >
      {children}
    </div>
  )
}
