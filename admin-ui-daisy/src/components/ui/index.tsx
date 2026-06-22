import { Children, createContext, isValidElement, useCallback, useContext, useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { AlertTriangle, Check, ChevronDown, X } from 'lucide-react'
import { Alert, Badge as DaisyBadge, Button, Card, Loading, Modal } from 'react-daisyui'

// ============================================================================
// Select - 自定义选择器，避免原生 select 组件
// ============================================================================

type SelectChangeEvent = { target: { value: string } }

interface SelectOptionProps {
  value: string
  disabled?: boolean
  children: ReactNode
}

interface SelectRootProps {
  value?: string
  disabled?: boolean
  children: ReactNode
  className?: string
  size?: 'xs' | 'sm' | 'md' | 'lg'
  bordered?: boolean
  onChange?: (event: SelectChangeEvent) => void
}

function SelectOption(_props: SelectOptionProps) {
  return null
}

function optionText(value: ReactNode): string {
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(optionText).join('')
  return ''
}

function SelectRoot({ value = '', disabled, children, className = '', size = 'md', onChange }: SelectRootProps) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  const options = Children.toArray(children)
    .filter(isValidElement<SelectOptionProps>)
    .map((child) => ({
      value: String(child.props.value),
      disabled: child.props.disabled,
      label: child.props.children,
      text: optionText(child.props.children),
    }))
  const selected = options.find((option) => option.value === String(value)) || options[0]

  useEffect(() => {
    if (!open) return
    const handlePointerDown = (event: PointerEvent) => {
      if (!ref.current?.contains(event.target as Node)) setOpen(false)
    }
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false)
    }
    document.addEventListener('pointerdown', handlePointerDown)
    document.addEventListener('keydown', handleKeyDown)
    return () => {
      document.removeEventListener('pointerdown', handlePointerDown)
      document.removeEventListener('keydown', handleKeyDown)
    }
  }, [open])

  const choose = (nextValue: string, optionDisabled?: boolean) => {
    if (disabled || optionDisabled) return
    onChange?.({ target: { value: nextValue } })
    setOpen(false)
  }

  return (
    <div ref={ref} className={`choice-select ${className}`} data-size={size}>
      <button
        type="button"
        className="choice-select-trigger"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={selected?.text}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="min-w-0 truncate">{selected?.label || '请选择'}</span>
        <ChevronDown className={`h-4 w-4 shrink-0 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>
      {open && !disabled && (
        <div className="choice-select-menu" role="listbox">
          {options.map((option) => {
            const active = option.value === String(value)
            return (
              <button
                key={option.value}
                type="button"
                role="option"
                aria-selected={active}
                disabled={option.disabled}
                className={`choice-select-option ${active ? 'is-active' : ''}`}
                title={option.text}
                onClick={() => choose(option.value, option.disabled)}
              >
                <span className="min-w-0 truncate">{option.label}</span>
                {active && <Check className="h-3.5 w-3.5 shrink-0" />}
              </button>
            )
          })}
        </div>
      )}
    </div>
  )
}

export const Select = Object.assign(SelectRoot, { Option: SelectOption })

// ============================================================================
// Confirm Dialog - 自定义确认弹窗，避免浏览器 confirm
// ============================================================================

interface ConfirmOptions {
  title: string
  message: ReactNode
  confirmText?: string
  cancelText?: string
  tone?: 'default' | 'danger'
}

type ConfirmRequest = ConfirmOptions & { resolve: (confirmed: boolean) => void }

const ConfirmContext = createContext<((options: ConfirmOptions) => Promise<boolean>) | null>(null)

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [request, setRequest] = useState<ConfirmRequest | null>(null)

  const confirm = useCallback((options: ConfirmOptions) => {
    return new Promise<boolean>((resolve) => {
      setRequest({ ...options, resolve })
    })
  }, [])

  const close = (confirmed: boolean) => {
    request?.resolve(confirmed)
    setRequest(null)
  }

  return (
    <ConfirmContext.Provider value={confirm}>
      {children}
      <ModalShell
        open={Boolean(request)}
        title={request?.title || '确认操作'}
        width="max-w-md"
        onClose={() => close(false)}
        footer={
          <>
            <Button type="button" variant="outline" size="sm" onClick={() => close(false)}>
              {request?.cancelText || '取消'}
            </Button>
            <Button type="button" color={request?.tone === 'danger' ? 'error' : 'primary'} size="sm" onClick={() => close(true)}>
              {request?.confirmText || '确认'}
            </Button>
          </>
        }
      >
        <div className="flex gap-3 text-sm leading-6 text-base-content/70">
          <span className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg ${request?.tone === 'danger' ? 'bg-error/10 text-error' : 'bg-primary/10 text-primary'}`}>
            <AlertTriangle className="h-4 w-4" />
          </span>
          <div className="min-w-0">{request?.message}</div>
        </div>
      </ModalShell>
    </ConfirmContext.Provider>
  )
}

export function useConfirm() {
  const confirm = useContext(ConfirmContext)
  if (!confirm) throw new Error('useConfirm must be used inside ConfirmProvider')
  return confirm
}

// ============================================================================
// Stat Card - 统计卡片
// ============================================================================

interface StatCardProps {
  title: string
  value: ReactNode
  desc?: ReactNode
  icon?: ReactNode
  tone?: 'default' | 'success' | 'warning' | 'error' | 'info' | 'primary' | 'secondary'
  trend?: { value: number; label?: string }
}

const toneStyles: Record<string, { text: string; accent: string; icon: string }> = {
  default: { text: 'text-base-content', accent: 'stat-card-accent', icon: 'text-base-content/55' },
  success: { text: 'text-success', accent: 'bg-success/75', icon: 'text-success' },
  warning: { text: 'text-warning', accent: 'bg-warning/75', icon: 'text-warning' },
  error: { text: 'text-error', accent: 'bg-error/75', icon: 'text-error' },
  info: { text: 'text-info', accent: 'bg-info/70', icon: 'text-info' },
  primary: { text: 'text-primary', accent: 'bg-primary/75', icon: 'text-primary' },
  secondary: { text: 'text-primary', accent: 'bg-primary/45', icon: 'text-primary' },
}

export function StatCard({ title, value, desc, icon, tone = 'default', trend }: StatCardProps) {
  const styles = toneStyles[tone]
  return (
    <Card className="stat-card overflow-hidden">
      <Card.Body className="relative gap-1 p-4">
        <div className={`absolute left-0 top-4 h-8 w-1 rounded-r-full ${styles.accent}`} />
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0 flex-1 pl-2.5">
            <div className="text-[0.72rem] font-semibold text-base-content/50">{title}</div>
            <div className={`mt-1 min-w-0 break-words text-2xl font-semibold tracking-tight ${styles.text}`}>{value}</div>
            {desc && <div className="mt-1 min-w-0 truncate text-[0.72rem] text-base-content/55">{desc}</div>}
          </div>
          {icon && <div className={`stat-card-icon shrink-0 rounded-md p-2 ${styles.icon}`}>{icon}</div>}
        </div>
        {trend && (
          <div className={`mt-2 pl-2.5 text-[0.7rem] font-medium ${trend.value >= 0 ? 'text-success' : 'text-error'}`}>
            {trend.value >= 0 ? '↑' : '↓'} {Math.abs(trend.value)}% {trend.label}
          </div>
        )}
      </Card.Body>
    </Card>
  )
}

// ============================================================================
// Section Card - 区块卡片
// ============================================================================

interface SectionCardProps {
  title?: ReactNode
  description?: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
  noPadding?: boolean
}

export function SectionCard({ title, description, actions, children, className = '', noPadding }: SectionCardProps) {
  return (
    <Card className={`section-card overflow-hidden ${className}`}>
      {(title || description || actions) && (
        <div className="section-card-header flex flex-col gap-3 px-4 py-3.5 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            {title && <h2 className="text-[0.95rem] font-semibold tracking-tight text-base-content">{title}</h2>}
            {description && <p className="mt-1 text-xs leading-5 text-base-content/55">{description}</p>}
          </div>
          {actions && <div className="flex shrink-0 flex-wrap items-center gap-1.5">{actions}</div>}
        </div>
      )}
      <div className={`section-card-body ${noPadding ? '' : 'p-4'}`}>{children}</div>
    </Card>
  )
}

// ============================================================================
// Modal Shell - 弹窗容器
// ============================================================================

interface ModalShellProps {
  open: boolean
  title: ReactNode
  children: ReactNode
  width?: string
  onClose: () => void
  footer?: ReactNode
}

export function ModalShell({ open, title, children, width = 'max-w-3xl', onClose, footer }: ModalShellProps) {
  if (!open) return null
  return (
    <Modal open backdrop className={`${width} rounded-box`}>
      <Modal.Header className="flex items-center justify-between gap-4 border-b border-base-300/60 pb-3">
        <h3 className="text-lg font-semibold">{title}</h3>
        <Button type="button" shape="circle" color="ghost" size="sm" onClick={onClose} aria-label="关闭">
          <X className="h-4 w-4" />
        </Button>
      </Modal.Header>
      <Modal.Body className="py-4">{children}</Modal.Body>
      {footer && <Modal.Actions className="border-t border-base-300/60 pt-3">{footer}</Modal.Actions>}
    </Modal>
  )
}

// ============================================================================
// Badge - 标签
// ============================================================================

interface BadgeProps {
  children: ReactNode
  tone?: 'neutral' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' | 'info' | 'accent'
  size?: 'xs' | 'sm'
  title?: string
  dot?: boolean
}

const badgeToneClass: Record<string, string> = {
  neutral: '!border-base-300 !bg-base-100/80 text-base-content/60',
  primary: '!border-primary/25 !bg-primary/10 text-primary',
  secondary: '!border-base-content/15 !bg-base-content/5 text-base-content/70',
  success: '!border-success/20 !bg-success/10 text-success',
  warning: '!border-warning/20 !bg-warning/10 text-warning',
  error: '!border-error/20 !bg-error/10 text-error',
  info: '!border-info/20 !bg-info/10 text-info',
  accent: '!border-primary/20 !bg-primary/10 text-primary',
}

export function Badge({ children, tone = 'neutral', size = 'sm', title, dot }: BadgeProps) {
  return (
    <DaisyBadge
      size={size}
      color="ghost"
      className={`gap-1 border font-semibold ${badgeToneClass[tone]} ${size === 'xs' ? 'h-4 px-1.5 text-[0.62rem]' : 'h-5 px-2 text-[0.68rem]'}`}
      title={title}
    >
      {dot && <span className={`h-1.5 w-1.5 rounded-full bg-current`} />}
      {children}
    </DaisyBadge>
  )
}

// ============================================================================
// Field Label - 表单标签
// ============================================================================

interface FieldLabelProps {
  title: string
  description?: string
  required?: boolean
  children: ReactNode
}

export function FieldLabel({ title, description, required, children }: FieldLabelProps) {
  return (
    <label className="form-control min-w-0">
      <span className="label-text mb-1 flex items-center gap-1 text-xs font-semibold text-base-content/70">
        {title}
        {required && <span className="text-error">*</span>}
      </span>
      {children}
      {description && (
        <span className="label-text-alt mt-1 block text-[0.68rem] leading-4 text-base-content/50">{description}</span>
      )}
    </label>
  )
}

// ============================================================================
// Empty State - 空状态
// ============================================================================

interface EmptyStateProps {
  icon?: ReactNode
  title?: string
  text?: string // backward compatibility
  description?: string
  action?: ReactNode
}

export function EmptyState({ icon, title, text, description, action }: EmptyStateProps) {
  const displayTitle = title || text || '暂无数据'
  return (
    <Card bordered className="setting-card border-dashed bg-base-100/70">
      <Card.Body className="items-center py-12 text-center">
        {icon && <div className="mb-3 text-base-content/30">{icon}</div>}
        <div className="text-sm font-semibold text-base-content/65">{displayTitle}</div>
        {description && <div className="mt-1 text-xs text-base-content/45">{description}</div>}
        {action && <div className="mt-4">{action}</div>}
      </Card.Body>
    </Card>
  )
}

// ============================================================================
// Loading State - 加载状态
// ============================================================================

interface LoadingStateProps {
  text?: string
  size?: 'sm' | 'md' | 'lg'
}

export function LoadingState({ text = '加载中...', size = 'sm' }: LoadingStateProps) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-12">
      <Loading size={size} className="text-primary" />
      <span className="text-sm text-base-content/50">{text}</span>
    </div>
  )
}

// ============================================================================
// Error State - 错误状态
// ============================================================================

interface ErrorStateProps {
  title?: string
  text?: string // backward compatibility
  message?: string
  action?: ReactNode
}

export function ErrorState({ title = '加载失败', text, message, action }: ErrorStateProps) {
  const displayMessage = message || text || '发生未知错误'
  return (
    <Alert status="error" className="flex-col items-start gap-2 text-sm">
      <div className="font-semibold">{title}</div>
      <div className="text-error/80">{displayMessage}</div>
      {action && <div className="mt-2">{action}</div>}
    </Alert>
  )
}

// ============================================================================
// Kbd - 键盘快捷键
// ============================================================================

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded border border-base-300 bg-base-200 px-1.5 py-0.5 font-mono text-[0.68rem] text-base-content/70">
      {children}
    </kbd>
  )
}

// ============================================================================
// Divider - 分割线
// ============================================================================

export function Divider({ label }: { label?: string }) {
  if (label) {
    return (
      <div className="flex items-center gap-3 py-2">
        <div className="h-px flex-1 bg-base-300" />
        <span className="text-xs text-base-content/40">{label}</span>
        <div className="h-px flex-1 bg-base-300" />
      </div>
    )
  }
  return <div className="my-3 h-px bg-base-300" />
}
