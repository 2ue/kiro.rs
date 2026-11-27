import * as React from 'react'
import * as DialogPrimitive from '@radix-ui/react-dialog'
import { cn } from '@/lib/utils'

const ModalOpenContext = React.createContext(false)

export function ModalRoot({
  open: controlledOpen,
  defaultOpen = false,
  onOpenChange,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Root>) {
  const [uncontrolledOpen, setUncontrolledOpen] = React.useState(defaultOpen)
  const open = controlledOpen ?? uncontrolledOpen

  return (
    <ModalOpenContext.Provider value={open}>
      <DialogPrimitive.Root
        {...props}
        open={open}
        onOpenChange={(next) => {
          if (controlledOpen === undefined) setUncontrolledOpen(next)
          onOpenChange?.(next)
        }}
      />
    </ModalOpenContext.Provider>
  )
}

export function ModalPortal({
  children,
  layer = 'default',
}: {
  children: React.ReactNode
  layer?: 'default' | 'confirm'
}) {
  const open = React.useContext(ModalOpenContext)

  return (
    <DialogPrimitive.Portal>
      <div
        data-modal-layer={layer}
        data-state={open ? 'open' : 'closed'}
        className={cn(
          'pointer-events-none fixed inset-0 isolate',
          // Keep the portal mounted while its overlay and content finish their exit animations.
          'data-[state=closed]:animate-out data-[state=closed]:duration-200',
          layer === 'confirm' ? 'z-[80]' : 'z-50'
        )}
      >
        {children}
      </div>
    </DialogPrimitive.Portal>
  )
}
