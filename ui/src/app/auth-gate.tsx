import * as React from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/http'
import { extractErrorMessage } from '@/lib/utils'
import { LoginPage } from '@/features/auth/login-page'
import { Spinner } from '@/components/ui'

type AuthState = 'checking' | 'authed' | 'guest'

const AuthContext = React.createContext<{ logout: () => void }>({ logout: () => {} })
export const useAuth = () => React.useContext(AuthContext)

export function AuthGate({ children }: { children: React.ReactNode }) {
  const queryClient = useQueryClient()
  const [state, setState] = React.useState<AuthState>('checking')
  const [authError, setAuthError] = React.useState('')

  React.useEffect(() => {
    let cancelled = false
    const savedKey = storage.getApiKey()
    if (!savedKey) {
      setState('guest')
      return
    }
    validateAdminApiKey(savedKey)
      .then(() => {
        if (!cancelled) {
          setAuthError('')
          setState('authed')
        }
      })
      .catch((error) => {
        storage.removeApiKey()
        if (!cancelled) {
          setAuthError(extractErrorMessage(error))
          setState('guest')
        }
      })
    return () => {
      cancelled = true
    }
  }, [])

  React.useEffect(() => {
    const handleAuthFailed = () => {
      storage.removeApiKey()
      queryClient.clear()
      setAuthError('登录已失效，请重新输入管理后台 Key')
      setState('guest')
    }
    window.addEventListener('kiro-admin-auth-failed', handleAuthFailed)
    return () => window.removeEventListener('kiro-admin-auth-failed', handleAuthFailed)
  }, [queryClient])

  const logout = React.useCallback(() => {
    storage.removeApiKey()
    queryClient.clear()
    setState('guest')
  }, [queryClient])

  if (state === 'checking') {
    return (
      <div className="flex min-h-screen items-center justify-center bg-background">
        <div className="flex flex-col items-center gap-4 rounded-xl border border-border bg-card px-8 py-7 shadow-sm">
          <Spinner size="lg" />
          <p className="text-sm text-muted-foreground">验证登录状态...</p>
        </div>
      </div>
    )
  }

  if (state === 'guest') {
    return (
      <LoginPage
        initialError={authError}
        onLogin={() => {
          setAuthError('')
          setState('authed')
        }}
      />
    )
  }

  return <AuthContext.Provider value={{ logout }}>{children}</AuthContext.Provider>
}
