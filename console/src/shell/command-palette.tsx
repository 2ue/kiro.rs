import { createContext, lazy, Suspense, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'

const CommandPaletteDialog = lazy(() => import('./command-palette-dialog'))

export interface PaletteAction {
  id: string
  label: string
  group: string
  icon?: ReactNode
  keywords?: string
  run: () => void
}

interface PaletteCtx {
  open: () => void
  register: (actions: PaletteAction[]) => () => void
}

const Ctx = createContext<PaletteCtx | null>(null)

export function CommandPaletteProvider({ children }: { children: ReactNode }) {
  const [isOpen, setOpen] = useState(false)
  const [everOpened, setEverOpened] = useState(false)
  const [registered, setRegistered] = useState<Map<string, PaletteAction[]>>(new Map())

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setEverOpened(true)
        setOpen((v) => !v)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const register = useCallback((actions: PaletteAction[]) => {
    const key = Math.random().toString(36).slice(2)
    setRegistered((prev) => new Map(prev).set(key, actions))
    return () =>
      setRegistered((prev) => {
        const next = new Map(prev)
        next.delete(key)
        return next
      })
  }, [])

  const ctx = useMemo(
    () => ({
      open: () => {
        setEverOpened(true)
        setOpen(true)
      },
      register,
    }),
    [register],
  )

  return (
    <Ctx.Provider value={ctx}>
      {children}
      {everOpened && (
        <Suspense fallback={null}>
          <CommandPaletteDialog open={isOpen} setOpen={setOpen} actions={[...registered.values()].flat()} />
        </Suspense>
      )}
    </Ctx.Provider>
  )
}

export function useCommandPalette() {
  const ctx = useContext(Ctx)
  if (!ctx) throw new Error('useCommandPalette must be used within CommandPaletteProvider')
  return ctx
}

/** 页面注册上下文动作到命令面板；依赖变化时重新注册 */
export function usePaletteActions(actions: PaletteAction[], deps: unknown[]) {
  const { register } = useCommandPalette()
  useEffect(() => register(actions), deps) // eslint-disable-line react-hooks/exhaustive-deps
}
