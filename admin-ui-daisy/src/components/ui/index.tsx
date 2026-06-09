import type { ReactNode } from 'react'
import { X } from 'lucide-react'
import { Alert, Badge as DaisyBadge, Button, Card, Loading, Modal } from 'react-daisyui'

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

const toneStyles: Record<string, { text: string; accent: string; bg: string; icon: string }> = {
  default: { text: 'text-base-content', accent: 'bg-base-content/20', bg: 'bg-base-100', icon: 'text-base-content/40' },
  success: { text: 'text-success', accent: 'bg-success/80', bg: 'bg-base-100', icon: 'text-success' },
  warning: { text: 'text-warning', accent: 'bg-warning/80', bg: 'bg-base-100', icon: 'text-warning' },
  error: { text: 'text-error', accent: 'bg-error/80', bg: 'bg-base-100', icon: 'text-error' },
  info: { text: 'text-info', accent: 'bg-info/70', bg: 'bg-base-100', icon: 'text-info' },
  primary: { text: 'text-primary', accent: 'bg-primary/75', bg: 'bg-base-100', icon: 'text-primary' },
  secondary: { text: 'text-primary', accent: 'bg-primary/45', bg: 'bg-base-100', icon: 'text-primary' },
}

export function StatCard({ title, value, desc, icon, tone = 'default', trend }: StatCardProps) {
  const styles = toneStyles[tone]
  return (
    <Card className={`stat-card overflow-hidden ${styles.bg}`}>
      <Card.Body className="relative gap-1 p-3">
        <div className={`absolute bottom-3 left-0 top-3 w-1 rounded-r-full ${styles.accent}`} />
        <div className="flex items-start justify-between gap-2">
          <div className="min-w-0 flex-1 pl-2">
            <div className="text-[0.7rem] font-semibold uppercase tracking-wide text-base-content/50">{title}</div>
            <div className={`mt-0.5 truncate text-xl font-bold tracking-tight ${styles.text}`}>{value}</div>
            {desc && <div className="mt-0.5 truncate text-[0.7rem] text-base-content/50">{desc}</div>}
          </div>
          {icon && <div className={`shrink-0 ${styles.icon}`}>{icon}</div>}
        </div>
        {trend && (
          <div className={`mt-1 pl-2 text-[0.68rem] font-medium ${trend.value >= 0 ? 'text-success' : 'text-error'}`}>
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
        <div className="section-card-header flex flex-col gap-3 border-b border-base-300/60 px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            {title && <h2 className="text-sm font-semibold tracking-tight">{title}</h2>}
            {description && <p className="mt-0.5 text-xs text-base-content/50">{description}</p>}
          </div>
          {actions && <div className="flex shrink-0 flex-wrap items-center gap-1.5">{actions}</div>}
        </div>
      )}
      <div className={noPadding ? '' : 'p-4'}>{children}</div>
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
    <Modal open backdrop className={`${width} rounded-2xl`}>
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
  neutral: '!border-base-300 !bg-base-100 text-base-content/55',
  primary: '!border-base-300 !bg-base-100 text-primary',
  secondary: '!border-base-300 !bg-base-100 text-base-content/60',
  success: '!border-base-300 !bg-base-100 text-success',
  warning: '!border-base-300 !bg-base-100 text-warning',
  error: '!border-base-300 !bg-base-100 text-error',
  info: '!border-base-300 !bg-base-100 text-info',
  accent: '!border-base-300 !bg-base-100 text-primary',
}

export function Badge({ children, tone = 'neutral', size = 'sm', title, dot }: BadgeProps) {
  return (
    <DaisyBadge
      size={size}
      color="ghost"
      className={`gap-1 border font-medium ${badgeToneClass[tone]} ${size === 'xs' ? 'h-4 px-1.5 text-[0.62rem]' : 'h-5 px-2 text-[0.68rem]'}`}
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
    <Card bordered className="border-dashed bg-base-200/40">
      <Card.Body className="items-center py-12 text-center">
        {icon && <div className="mb-3 text-base-content/30">{icon}</div>}
        <div className="text-sm font-medium text-base-content/60">{displayTitle}</div>
        {description && <div className="mt-1 text-xs text-base-content/40">{description}</div>}
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
