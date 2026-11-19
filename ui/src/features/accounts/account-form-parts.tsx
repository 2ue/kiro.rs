// 账号页弹窗共用的表单小件：分区、勾选才生效的字段、限宽数字框（单位在右）、开关格。
import type { ReactNode } from 'react'
import { Checkbox, Input, Switch } from '@/components/ui'
import { cn } from '@/lib/utils'

/** 大弹窗里的一个分区，与 main 批量弹窗的 FormSection 一致。 */
export function FormSection({ title, description, actions, children, className }: {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
}) {
  return (
    <section className={cn('rounded-lg border border-border/70 bg-card p-4', className)}>
      <div className="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold">{title}</h3>
          {description && <p className="mt-0.5 text-xs leading-5 text-muted-foreground">{description}</p>}
        </div>
        {actions && <div className="flex shrink-0 items-center gap-1">{actions}</div>}
      </div>
      {children}
    </section>
  )
}

/** 批量修改里的一个字段：勾上左侧框才修改。 */
export function BatchField({ id, label, description, enabled, onEnabledChange, children }: {
  id: string
  label: string
  description?: ReactNode
  enabled: boolean
  onEnabledChange: (enabled: boolean) => void
  children: ReactNode
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <label htmlFor={id} className="flex cursor-pointer items-center gap-2 text-sm font-medium">
        <Checkbox id={id} checked={enabled} onCheckedChange={(v) => onEnabledChange(v === true)} aria-label={`修改${label}`} />
        {label}
      </label>
      <div className={cn(!enabled && 'pointer-events-none opacity-50')}>{children}</div>
      {description && <p className="text-[0.7rem] leading-4 text-muted-foreground">{description}</p>}
    </div>
  )
}

/** 分区标题上的勾选框：勾上整个分区才生效。 */
export function SectionToggle({ id, checked, onChange, children }: { id: string; checked: boolean; onChange: (checked: boolean) => void; children: ReactNode }) {
  return (
    <label htmlFor={id} className="flex cursor-pointer items-center gap-2">
      <Checkbox id={id} checked={checked} onCheckedChange={(v) => onChange(v === true)} />
      {children}
    </label>
  )
}

/** 数字输入：限宽，单位放右侧。 */
export function NumberInput({ id, value, onChange, unit, step = '1', min, max, placeholder, disabled }: {
  id?: string
  value: string
  onChange: (value: string) => void
  unit?: string
  step?: string
  min?: number
  max?: number
  placeholder?: string
  disabled?: boolean
}) {
  return (
    <div className="flex min-h-9 items-center gap-2">
      <Input
        id={id}
        type="number"
        inputMode="decimal"
        className="w-32 shrink-0 tabular-nums"
        step={step}
        min={min}
        max={max}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
      {unit && <span className="shrink-0 text-xs text-muted-foreground">{unit}</span>}
    </div>
  )
}

/** 开关格：与输入框同样的高度和排版。 */
export function SwitchCell({ id, checked, onChange, onText = '开启', offText = '关闭', disabled }: {
  id?: string
  checked: boolean
  onChange: (checked: boolean) => void
  onText?: string
  offText?: string
  disabled?: boolean
}) {
  return (
    <div className="flex min-h-9 items-center gap-2">
      <Switch id={id} checked={checked} disabled={disabled} onCheckedChange={onChange} />
      <span className="text-xs text-muted-foreground">{checked ? onText : offText}</span>
    </div>
  )
}
