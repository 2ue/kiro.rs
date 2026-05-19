import { useState, type FormEvent } from 'react'
import { Navigate, useNavigate } from 'react-router-dom'
import { Loader2, ShieldCheck } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { adminApi } from '@/lib/api'
import { storage } from '@/lib/storage'
import { extractErrorMessage } from '@/lib/utils'
import { useAuth } from '@/store/auth'

export function LoginPage() {
  const isAuthed = useAuth((s) => s.isAuthed)
  const login = useAuth((s) => s.login)
  const [apiKey, setApiKey] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const navigate = useNavigate()

  if (isAuthed) {
    return <Navigate to="/dashboard" replace />
  }

  const onSubmit = async (e: FormEvent) => {
    e.preventDefault()
    if (!apiKey.trim()) {
      toast.error('请输入 Admin API Key')
      return
    }
    setSubmitting(true)
    try {
      // 暂存 key 让拦截器带上,验证一次后端;失败再清除
      storage.setApiKey(apiKey.trim())
      await adminApi.get('/credentials', { params: { _verify: 1 } }).catch((err) => {
        // credentials-paged 不存在 _verify 参数,忽略类型;只关心 401
        const status = err?.response?.status
        if (status === 401 || status === 403) {
          throw err
        }
      })
      login(apiKey.trim())
      toast.success('登录成功')
      navigate('/dashboard', { replace: true })
    } catch (err) {
      storage.removeApiKey()
      toast.error(`登录失败: ${extractErrorMessage(err)}`)
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="grid min-h-screen place-items-center bg-gradient-to-br from-muted/30 via-background to-muted/30 p-4">
      <Card className="w-full max-w-md">
        <CardHeader className="space-y-2 text-center">
          <div className="mx-auto flex h-12 w-12 items-center justify-center rounded-full bg-primary text-primary-foreground">
            <ShieldCheck className="h-6 w-6" />
          </div>
          <CardTitle className="text-xl">Kiro Console</CardTitle>
          <CardDescription>请输入 Admin API Key 登录管理控制台</CardDescription>
        </CardHeader>
        <form onSubmit={onSubmit}>
          <CardContent className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="api-key">Admin API Key</Label>
              <Input
                id="api-key"
                type="password"
                autoComplete="current-password"
                placeholder="sk-admin-..."
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                disabled={submitting}
              />
              <p className="text-xs text-muted-foreground">
                密钥保存在浏览器本地,不会发送到第三方
              </p>
            </div>
          </CardContent>
          <CardFooter>
            <Button type="submit" className="w-full" disabled={submitting}>
              {submitting && <Loader2 className="h-4 w-4 animate-spin" />}
              登录
            </Button>
          </CardFooter>
        </form>
      </Card>
    </div>
  )
}
