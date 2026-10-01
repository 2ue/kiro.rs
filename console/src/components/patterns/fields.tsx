import { useEffect, useState, type ReactNode } from 'react'
import { RotateCcw } from 'lucide-react'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

interface RowProps {
  label: ReactNode
  description?: ReactNode
  /** 与服务端值不同（草稿已修改） */
  dirty?: boolean
  onReset?: () => void
  htmlFor?: string
  disabled?: boolean
  className?: string
  /** 控件放在说明下方（文本域等） */
  stacked?: boolean
  children: ReactNode
}

/** 设置项行：左侧标签与说明，右侧控件；修改过的字段左侧显示状态条 */
export function SettingRow({ label, description, dirty, onReset, htmlFor, disabled, className, stacked, children }: RowProps) {
  return (
    <div
      className={cn(
        'relative flex gap-x-6 gap-y-2 py-3 pl-3',
        stacked ? 'flex-col' : 'flex-col sm:flex-row sm:items-start sm:justify-between',
        disabled && 'opacity-55',
        className,
      )}
    >
      {dirty && <span className="absolute inset-y-3 left-0 w-0.5 rounded-full bg-primary" aria-label="已修改" />}
      <div className="min-w-0 flex-1">
        <label htmlFor={htmlFor} className="flex items-center gap-1.5 text-sm font-medium">
          {label}
          {dirty && onReset && (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon-xs" onClick={onReset} aria-label="撤销此项修改">
                  <RotateCcw />
                </Button>
              </TooltipTrigger>
              <TooltipContent>撤销此项修改</TooltipContent>
            </Tooltip>
          )}
        </label>
        {description && <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{description}</p>}
      </div>
      <div className={cn('shrink-0', !stacked && 'sm:w-56')}>{children}</div>
    </div>
  )
}

/** 数字输入：本地保留文本，失焦时提交并按 min/max 规整，避免输入过程被打断 */
export function NumberInput({
  id,
  value,
  onChange,
  min,
  max,
  step,
  suffix,
  disabled,
  allowEmpty,
  placeholder,
  className,
}: {
  id?: string
  value: number | null | undefined
  onChange: (value: number | null) => void
  min?: number
  max?: number
  step?: number
  suffix?: string
  disabled?: boolean
  /** 允许清空为 null（例如"继承全局"） */
  allowEmpty?: boolean
  placeholder?: string
  className?: string
}) {
  const [text, setText] = useState(value === null || value === undefined ? '' : String(value))
  useEffect(() => {
    setText(value === null || value === undefined ? '' : String(value))
  }, [value])

  const commit = () => {
    const trimmed = text.trim()
    if (trimmed === '') {
      if (allowEmpty) onChange(null)
      else setText(value === null || value === undefined ? '' : String(value))
      return
    }
    let next = Number(trimmed)
    if (!Number.isFinite(next)) {
      setText(value === null || value === undefined ? '' : String(value))
      return
    }
    if (typeof min === 'number') next = Math.max(min, next)
    if (typeof max === 'number') next = Math.min(max, next)
    if (!step || Number.isInteger(step)) next = Math.round(next)
    setText(String(next))
    if (next !== value) onChange(next)
  }

  return (
    <div className={cn('relative', className)}>
      <Input
        id={id}
        inputMode="decimal"
        value={text}
        disabled={disabled}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit()
        }}
        className={cn('num text-right', suffix && 'pr-14')}
      />
      {suffix && (
        <span className="pointer-events-none absolute inset-y-0 right-2.5 flex items-center text-xs text-muted-foreground">{suffix}</span>
      )}
    </div>
  )
}

export function ToggleControl({
  id,
  checked,
  onChange,
  disabled,
}: {
  id?: string
  checked: boolean
  onChange: (value: boolean) => void
  disabled?: boolean
}) {
  return (
    <div className="flex sm:justify-end">
      <Switch id={id} checked={checked} onCheckedChange={onChange} disabled={disabled} />
    </div>
  )
}

export function SelectControl<T extends string>({
  id,
  value,
  onChange,
  options,
  disabled,
  className,
}: {
  id?: string
  value: T
  onChange: (value: T) => void
  options: Array<{ value: T; label: string }>
  disabled?: boolean
  className?: string
}) {
  return (
    <Select value={value} onValueChange={(v) => onChange(v as T)} disabled={disabled}>
      <SelectTrigger id={id} className={cn('w-full', className)}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((option) => (
          <SelectItem key={option.value} value={option.value}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

/** 多行列表输入：每行一个值；失焦时提交 */
export function ListInput({
  id,
  value,
  onChange,
  disabled,
  placeholder,
  rows = 4,
  parse = (raw) =>
    raw
      .split(/[\r\n,]+/)
      .map((v) => v.trim())
      .filter(Boolean),
}: {
  id?: string
  value: string[]
  onChange: (value: string[]) => void
  disabled?: boolean
  placeholder?: string
  rows?: number
  parse?: (raw: string) => string[]
}) {
  const [text, setText] = useState(value.join('\n'))
  useEffect(() => {
    setText(value.join('\n'))
  }, [value])
  return (
    <Textarea
      id={id}
      rows={rows}
      value={text}
      disabled={disabled}
      placeholder={placeholder}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => {
        const next = parse(text)
        if (next.join('\n') !== value.join('\n')) onChange(next)
      }}
      className="font-mono text-xs"
    />
  )
}
