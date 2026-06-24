import * as React from 'react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogTitle,
  DialogDescription,
} from '@/components/ui'

/** 弹窗外壳:统一受控 Dialog(标题/描述/body/footer 三段式) */
interface ModalShellProps {
  open: boolean
  onClose: () => void
  title: React.ReactNode
  description?: React.ReactNode
  children: React.ReactNode
  footer?: React.ReactNode
  width?: string
  /** body 不要默认内边距 */
  noBodyPadding?: boolean
}

export function ModalShell({
  open,
  onClose,
  title,
  description,
  children,
  footer,
  width = 'max-w-2xl',
  noBodyPadding,
}: ModalShellProps) {
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      {open && (
        <DialogContent width={width}>
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description && <DialogDescription>{description}</DialogDescription>}
          </DialogHeader>
          <DialogBody className={noBodyPadding ? 'p-0' : undefined}>{children}</DialogBody>
          {footer && <DialogFooter>{footer}</DialogFooter>}
        </DialogContent>
      )}
    </Dialog>
  )
}
