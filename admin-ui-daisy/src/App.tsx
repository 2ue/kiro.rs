import { useCallback, useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { LoginPage } from '@/components/LoginPage'
import { Dashboard } from '@/components/Dashboard'
import { storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import { getStoredTheme } from '@/types/ui'

export default function App() {
  const queryClient = useQueryClient()
  const [loggedIn, setLoggedIn] = useState(false)
  const [checkingAuth, setCheckingAuth] = useState(true)
  const [authError, setAuthError] = useState('')

  useEffect(() => {
    const theme = getStoredTheme()
    document.documentElement.dataset.theme = theme
    localStorage.setItem('kiro-theme', theme)
  }, [])

  useEffect(() => {
    let cancelled = false
    const savedKey = storage.getApiKey()

    if (!savedKey) {
      setCheckingAuth(false)
      return
    }

    validateAdminApiKey(savedKey)
      .then(() => {
        if (!cancelled) {
          setAuthError('')
          setLoggedIn(true)
        }
      })
      .catch((error) => {
        storage.removeApiKey()
        if (!cancelled) {
          setAuthError(extractErrorMessage(error))
          setLoggedIn(false)
        }
      })
      .finally(() => {
        if (!cancelled) {
          setCheckingAuth(false)
        }
      })

    return () => {
      cancelled = true
    }
  }, [])

  const logout = useCallback(() => {
    setLoggedIn(false)
  }, [])

  useEffect(() => {
    const handleAuthFailed = () => {
      storage.removeApiKey()
      queryClient.clear()
      setAuthError('登录已失效，请重新输入管理后台 Key（adminApiKey）')
      setLoggedIn(false)
    }

    window.addEventListener('kiro-admin-auth-failed', handleAuthFailed)
    return () => window.removeEventListener('kiro-admin-auth-failed', handleAuthFailed)
  }, [queryClient])

  if (checkingAuth) {
    return (
      <main className="flex min-h-screen items-center justify-center bg-base-200">
        <div className="text-center">
          <span className="loading loading-spinner loading-lg text-primary" />
          <p className="mt-4 text-sm text-base-content/60">验证登录状态...</p>
        </div>
      </main>
    )
  }

  return loggedIn ? (
    <Dashboard onLogout={logout} />
  ) : (
    <LoginPage
      initialError={authError}
      onLogin={() => {
        setAuthError('')
        setLoggedIn(true)
      }}
    />
  )
}
