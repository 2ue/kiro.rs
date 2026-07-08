import { useState, useEffect } from 'react'
import { KeyRound } from 'lucide-react'
import { ADMIN_API_KEY_FIELD, storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'

interface LoginPageProps {
  onLogin: (adminApiKey: string) => void
  initialError?: string
}

export function LoginPage({ onLogin, initialError = '' }: LoginPageProps) {
  const [adminApiKey, setAdminApiKey] = useState('')
  const [error, setError] = useState(initialError)
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
    const savedAdminApiKey = storage.getApiKey()
    if (savedAdminApiKey) {
      setAdminApiKey(savedAdminApiKey)
    }
  }, [])

  useEffect(() => {
    setError(initialError)
  }, [initialError])

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    const trimmed = adminApiKey.trim()
    if (!trimmed) {
      setError(`请输入管理后台 Key（${ADMIN_API_KEY_FIELD}）`)
      return
    }

    setSubmitting(true)
    setError('')
    try {
      await validateAdminApiKey(trimmed)
      storage.setApiKey(trimmed)
      onLogin(trimmed)
    } catch (error) {
      storage.removeApiKey()
      setError(extractErrorMessage(error))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-background p-4">
      <Card className="w-full max-w-md">
        <CardHeader className="text-center">
          <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full bg-primary/10">
            <KeyRound className="h-6 w-6 text-primary" />
          </div>
          <CardTitle className="text-2xl">Kiro Admin</CardTitle>
          <CardDescription>
            请输入启动配置里的管理后台 Key（{ADMIN_API_KEY_FIELD}）以访问管理面板
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Input
                type="password"
                placeholder={`管理后台 Key（${ADMIN_API_KEY_FIELD}）`}
                value={adminApiKey}
                onChange={(e) => {
                  setAdminApiKey(e.target.value)
                  setError('')
                }}
                className="text-center"
              />
              <p className="text-xs leading-5 text-muted-foreground">
                对应配置字段 <span className="font-mono">{ADMIN_API_KEY_FIELD}</span>，请求时会作为 <span className="font-mono">x-api-key</span> 发送；它不是 Kiro 凭据的 API Key。
              </p>
            </div>
            {error && (
              <div className="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error}
              </div>
            )}
            <Button type="submit" className="w-full" disabled={!adminApiKey.trim() || submitting}>
              {submitting ? '验证中...' : '登录'}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
