import { useState, useEffect, useCallback } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/credentials'
import { LoginPage } from '@/components/login-page'
import { Dashboard } from '@/components/dashboard'
import { Toaster } from '@/components/ui/sonner'
import { extractErrorMessage } from '@/lib/utils'

function App() {
  const queryClient = useQueryClient()
  const [isLoggedIn, setIsLoggedIn] = useState(false)
  const [checkingAuth, setCheckingAuth] = useState(true)
  const [authError, setAuthError] = useState('')

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
          setIsLoggedIn(true)
        }
      })
      .catch((error) => {
        storage.removeApiKey()
        if (!cancelled) {
          setAuthError(extractErrorMessage(error))
          setIsLoggedIn(false)
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

  const handleLogin = () => {
    setAuthError('')
    setIsLoggedIn(true)
  }

  const handleLogout = useCallback(() => {
    setIsLoggedIn(false)
  }, [])

  useEffect(() => {
    const handleAuthFailed = () => {
      storage.removeApiKey()
      queryClient.clear()
      setAuthError('登录已失效，请重新输入管理后台 Key（adminApiKey）')
      setIsLoggedIn(false)
    }

    window.addEventListener('kiro-admin-auth-failed', handleAuthFailed)
    return () => window.removeEventListener('kiro-admin-auth-failed', handleAuthFailed)
  }, [queryClient])

  return (
    <>
      {checkingAuth ? (
        <div className="min-h-screen flex items-center justify-center bg-background">
          <div className="text-center">
            <div className="animate-spin rounded-full h-10 w-10 border-b-2 border-primary mx-auto mb-4"></div>
            <p className="text-muted-foreground">验证登录状态...</p>
          </div>
        </div>
      ) : isLoggedIn ? (
        <Dashboard onLogout={handleLogout} />
      ) : (
        <LoginPage onLogin={handleLogin} initialError={authError} />
      )}
      <Toaster position="top-right" />
    </>
  )
}

export default App
