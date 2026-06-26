import { type ReactNode } from 'react'
import { Input, Label, Switch, Textarea } from '@/components/ui'
import {
  Select,
  SelectContent,
  SelectTrigger,
  SelectValue,
} from '@/components/ui'
import { cn } from '@/lib/utils'

// ============================================================================
// FormSection
// ============================================================================

export function FormSection({ title, description, children }: {
  title: string
  description?: string
  children: ReactNode
}) {
  return (
    <section className="rounded-lg border border-border bg-card p-3">
      <div className="mb-3">
        <div className="text-sm font-semibold">{title}</div>
        {description && <p className="mt-1 text-xs leading-5 text-muted-foreground">{description}</p>}
      </div>
      {children}
    </section>
  )
}

// ============================================================================
// HintBox
// ============================================================================

export function HintBox({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-lg border border-border bg-muted/50 px-3 py-2 text-xs leading-5 text-muted-foreground">
      {children}
    </div>
  )
}

// ============================================================================
// ToggleRow
// ============================================================================

export function ToggleRow({ label, checked, disabled = false, onChange }: {
  label: string
  checked: boolean
  disabled?: boolean
  onChange: (v: boolean) => void
}) {
  return (
    <label className={cn(
      'flex min-h-12 items-center justify-between gap-3 rounded-lg border border-border bg-card px-3 py-2 text-sm',
      disabled && 'cursor-not-allowed opacity-60',
    )}>
      <span className="min-w-0 font-medium text-muted-foreground">{label}</span>
      <Switch className="shrink-0" checked={checked} disabled={disabled} onCheckedChange={onChange} />
    </label>
  )
}

// ============================================================================
// NumberBox
// ============================================================================

export function NumberBox({ label, description, value, min = 0, disabled = false, suffix, onChange }: {
  label: string
  description?: string
  value: number
  min?: number
  disabled?: boolean
  suffix?: string
  onChange: (v: number) => void
}) {
  return (
    <div>
      <div className="mb-1">
        <Label>{label}</Label>
        {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      </div>
      <div className="flex items-center gap-1">
        <Input
          type="number"
          min={min}
          inputMode="numeric"
          className="h-9 w-full text-sm"
          value={value}
          disabled={disabled}
          onChange={(e) => onChange(Number(e.target.value))}
        />
        {suffix && <span className="shrink-0 text-xs text-muted-foreground">{suffix}</span>}
      </div>
    </div>
  )
}

// ============================================================================
// TextBox
// ============================================================================

export function TextBox({ label, description, value, disabled = false, className, onChange }: {
  label: string
  description?: string
  value: string
  disabled?: boolean
  className?: string
  onChange: (v: string) => void
}) {
  return (
    <div className={className}>
      <div className="mb-1">
        <Label>{label}</Label>
        {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      </div>
      <Input className="h-9 w-full text-sm" value={value} disabled={disabled} onChange={(e) => onChange(e.target.value)} />
    </div>
  )
}

// ============================================================================
// SelectBox
// ============================================================================

export function SelectBox({ label, value, disabled = false, onChange, children }: {
  label: string
  value: string
  disabled?: boolean
  onChange: (v: string) => void
  children: ReactNode
}) {
  return (
    <div>
      <div className="mb-1"><Label>{label}</Label></div>
      <Select value={value} onValueChange={onChange} disabled={disabled}>
        <SelectTrigger size="sm" className="w-full"><SelectValue /></SelectTrigger>
        <SelectContent>{children}</SelectContent>
      </Select>
    </div>
  )
}

// ============================================================================
// TextAreaBox
// action 插槽为可选，form-modal 使用，page 不传
// ============================================================================

export function TextAreaBox({ label, description, value, disabled = false, action, onChange }: {
  label: string
  description?: string
  value: string
  disabled?: boolean
  action?: ReactNode
  onChange: (v: string) => void
}) {
  return (
    <div>
      <div className="mb-1 flex items-start justify-between gap-3">
        <div>
          <Label>{label}</Label>
          {description && <div className="mt-0.5 text-xs leading-4 text-muted-foreground">{description}</div>}
        </div>
        {action}
      </div>
      <Textarea
        className="min-h-24 w-full font-mono text-xs"
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  )
}
