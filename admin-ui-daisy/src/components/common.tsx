import type { ReactNode } from 'react'
import { X } from 'lucide-react'
import {
  Alert,
  Badge as DaisyBadge,
  Button,
  Card,
  Loading,
  Modal,
} from 'react-daisyui'

export function StatCard({
  title,
  value,
  desc,
  tone = 'default',
}: {
  title: string
  value: ReactNode
  desc?: ReactNode
  tone?: 'default' | 'success' | 'warning' | 'error' | 'info'
}) {
  const toneClass: Record<string, { text: string; accent: string; bg: string }> = {
    default: { text: 'text-base-content', accent: 'bg-base-content/25', bg: 'bg-base-100' },
    success: { text: 'text-success', accent: 'bg-success', bg: 'bg-success/5' },
    warning: { text: 'text-warning', accent: 'bg-warning', bg: 'bg-warning/5' },
    error: { text: 'text-error', accent: 'bg-error', bg: 'bg-error/5' },
    info: { text: 'text-primary', accent: 'bg-primary', bg: 'bg-primary/5' },
  }
  return (
    <Card className={`metric-card ${toneClass[tone].bg}`}>
      <Card.Body className="relative p-3">
        <div className={`mb-2 h-0.5 w-8 rounded-full ${toneClass[tone].accent}`} />
        <div className="text-xs font-semibold text-base-content/55">{title}</div>
        <div className={`mt-0.5 truncate text-[1.35rem] font-semibold leading-tight tracking-tight ${toneClass[tone].text}`}>
          {value}
        </div>
        {desc && <div className="mt-0.5 truncate text-xs leading-4 text-base-content/58">{desc}</div>}
      </Card.Body>
    </Card>
  )
}

export function SectionCard({
  title,
  description,
  actions,
  children,
}: {
  title?: ReactNode
  description?: ReactNode
  actions?: ReactNode
  children: ReactNode
}) {
  return (
    <Card className="section-card">
      {(title || description || actions) && (
        <div className="section-card-header">
          <div className="flex flex-col gap-3 md:flex-row md:items-start md:justify-between">
            <div className="min-w-0">
              {title && <h2 className="text-sm font-semibold tracking-tight">{title}</h2>}
              {description && <p className="mt-0.5 text-xs leading-5 text-base-content/60">{description}</p>}
            </div>
            {actions && <div className="flex shrink-0 flex-wrap gap-1.5">{actions}</div>}
          </div>
        </div>
      )}
      <div className="p-4 max-sm:p-3">{children}</div>
    </Card>
  )
}

export function ModalShell({
  open,
  title,
  children,
  width = 'max-w-3xl',
  onClose,
}: {
  open: boolean
  title: ReactNode
  children: ReactNode
  width?: string
  onClose: () => void
}) {
  if (!open) return null
  return (
    <Modal open backdrop className={width}>
      <Modal.Header className="mb-4 flex items-start justify-between gap-4">
        <h3 className="text-lg font-semibold">{title}</h3>
        <Button type="button" shape="circle" color="ghost" size="sm" onClick={onClose} aria-label="关闭">
          <X className="h-4 w-4" />
        </Button>
      </Modal.Header>
      <Modal.Body>{children}</Modal.Body>
    </Modal>
  )
}

export function FieldLabel({
  title,
  description,
  children,
}: {
  title: string
  description?: string
  children: ReactNode
}) {
  return (
    <label className="form-control">
      <span className="label-text block pb-0.5 text-left text-xs font-semibold text-base-content/70">{title}</span>
      {children}
      {description && (
        <span className="label-text-alt block pt-0.5 text-left leading-4 text-base-content/56">{description}</span>
      )}
    </label>
  )
}

export function Badge({
  children,
  tone = 'neutral',
  title,
}: {
  children: ReactNode
  tone?: 'neutral' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' | 'info'
  title?: string
}) {
  const classes: Record<string, string> = {
    neutral: 'ghost',
    primary: 'primary',
    secondary: 'secondary',
    success: 'success',
    warning: 'warning',
    error: 'error',
    info: 'info',
  }
  return (
    <DaisyBadge size="sm" color={classes[tone] as never} className="h-5 gap-1 border px-2 text-[0.68rem]" title={title}>
      {children}
    </DaisyBadge>
  )
}

export function EmptyState({ text }: { text: string }) {
  return <Card bordered className="border-dashed bg-base-200/60"><Card.Body className="py-10 text-center text-sm text-base-content/60">{text}</Card.Body></Card>
}

export function LoadingState({ text = '加载中...' }: { text?: string }) {
  return (
    <div className="flex items-center justify-center gap-2 py-10 text-sm text-base-content/60">
      <Loading size="sm" />
      {text}
    </div>
  )
}

export function ErrorState({ text }: { text: string }) {
  return <Alert status="error" className="text-sm">{text}</Alert>
}
