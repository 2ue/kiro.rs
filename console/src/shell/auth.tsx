import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { AUTH_EXPIRED_EVENT, authStorage } from '@/lib/auth-storage'

interface AuthCtx {
  authed: boolean
  login: (key: string, remember: boolean) => void
  logout: () => void
}

const Ctx = createContext<AuthCtx | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const [authed, setAuthed] = useState(() => !!authStorage.get())
  const queryClient = useQueryClient()

  const logout = useCallback(() => {
    authStorage.clear()
    queryClient.clear()
    setAuthed(false)
  }, [queryClient])

  useEffect(() => {
    const onExpired = () => logout()
    window.addEventListener(AUTH_EXPIRED_EVENT, onExpired)
    return () => window.removeEventListener(AUTH_EXPIRED_EVENT, onExpired)
  }, [logout])

  const value = useMemo<AuthCtx>(
    () => ({
      authed,
      login: (key, remember) => {
        authStorage.set(key, remember)
        setAuthed(true)
      },
      logout,
    }),
    [authed, logout],
  )

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useAuth() {
  const ctx = useContext(Ctx)
  if (!ctx) throw new Error('useAuth must be used within AuthProvider')
  return ctx
}
