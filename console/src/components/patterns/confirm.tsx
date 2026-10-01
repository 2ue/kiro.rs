import { createContext, useCallback, useContext, useRef, useState, type ReactNode } from 'react'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Input } from '@/components/ui/input'
import { buttonVariants } from '@/components/ui/button'

export interface ConfirmOptions {
  title: string
  description?: ReactNode
  confirmText?: string
  /** 危险操作会使用红色按钮 */
  destructive?: boolean
  /** 需要用户输入该文本才能确认（用于批量删除等） */
  typeToConfirm?: string
}

type ConfirmFn = (options: ConfirmOptions) => Promise<boolean>

const ConfirmContext = createContext<ConfirmFn | null>(null)

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [options, setOptions] = useState<ConfirmOptions | null>(null)
  const [typed, setTyped] = useState('')
  const resolver = useRef<((value: boolean) => void) | null>(null)

  const confirm = useCallback<ConfirmFn>((next) => {
    setTyped('')
    setOptions(next)
    return new Promise<boolean>((resolve) => {
      resolver.current = resolve
    })
  }, [])

  const close = (value: boolean) => {
    resolver.current?.(value)
    resolver.current = null
    setOptions(null)
  }

  const blocked = !!options?.typeToConfirm && typed.trim() !== options.typeToConfirm

  return (
    <ConfirmContext.Provider value={confirm}>
      {children}
      <AlertDialog open={!!options} onOpenChange={(open) => !open && close(false)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{options?.title}</AlertDialogTitle>
            {options?.description && <AlertDialogDescription asChild><div>{options.description}</div></AlertDialogDescription>}
          </AlertDialogHeader>
          {options?.typeToConfirm && (
            <div className="space-y-1.5">
              <p className="text-xs text-muted-foreground">
                请输入 <span className="font-mono font-medium text-foreground">{options.typeToConfirm}</span> 以确认
              </p>
              <Input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus aria-label="确认文本" />
            </div>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              disabled={blocked}
              className={options?.destructive ? buttonVariants({ variant: 'destructive' }) : undefined}
              onClick={(e) => {
                if (blocked) {
                  e.preventDefault()
                  return
                }
                close(true)
              }}
            >
              {options?.confirmText ?? '确认'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </ConfirmContext.Provider>
  )
}

export function useConfirm(): ConfirmFn {
  const ctx = useContext(ConfirmContext)
  if (!ctx) throw new Error('useConfirm must be used within ConfirmProvider')
  return ctx
}
