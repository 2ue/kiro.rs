import * as React from 'react'
import { AlertTriangle } from 'lucide-react'
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogTitle,
} from '@/components/ui'

interface ConfirmOptions {
  title: string
  message: React.ReactNode
  confirmText?: string
  cancelText?: string
  tone?: 'default' | 'danger'
}

type ConfirmRequest = ConfirmOptions & { resolve: (confirmed: boolean) => void }

const ConfirmContext = React.createContext<((options: ConfirmOptions) => Promise<boolean>) | null>(
  null
)

export function ConfirmProvider({ children }: { children: React.ReactNode }) {
  const [request, setRequest] = React.useState<ConfirmRequest | null>(null)

  const confirm = React.useCallback((options: ConfirmOptions) => {
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
      <Dialog open={Boolean(request)} onOpenChange={(open) => !open && close(false)}>
        {request && (
          <DialogContent width="max-w-md">
            <DialogHeader>
              <DialogTitle>{request.title}</DialogTitle>
            </DialogHeader>
            <DialogBody>
              <div className="flex gap-3 text-sm leading-6 text-muted-foreground">
                <span
                  className={
                    'mt-0.5 flex size-9 shrink-0 items-center justify-center rounded-lg ' +
                    (request.tone === 'danger'
                      ? 'bg-destructive/10 text-destructive'
                      : 'bg-primary/10 text-primary')
                  }
                >
                  <AlertTriangle className="size-4" />
                </span>
                <div className="min-w-0 pt-1">{request.message}</div>
              </div>
            </DialogBody>
            <DialogFooter>
              <Button variant="outline" size="sm" onClick={() => close(false)}>
                {request.cancelText ?? '取消'}
              </Button>
              <Button
                variant={request.tone === 'danger' ? 'destructive' : 'default'}
                size="sm"
                onClick={() => close(true)}
              >
                {request.confirmText ?? '确认'}
              </Button>
            </DialogFooter>
          </DialogContent>
        )}
      </Dialog>
    </ConfirmContext.Provider>
  )
}

export function useConfirm() {
  const confirm = React.useContext(ConfirmContext)
  if (!confirm) throw new Error('useConfirm must be used inside ConfirmProvider')
  return confirm
}
