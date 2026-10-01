import { useState, type FormEvent } from 'react'
import { KeyRound, Loader2, ShieldAlert } from 'lucide-react'
import { ApiError, errorMessage } from '@/api/client'
import { systemApi } from '@/api/endpoints/system'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useAuth } from './auth'

export function LoginPage() {
  const { login } = useAuth()
  const [key, setKey] = useState('')
  const [remember, setRemember] = useState(false)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    const value = key.trim()
    if (!value) return
    setPending(true)
    setError(null)
    try {
      await systemApi.verifyKey(value)
      login(value, remember)
    } catch (err) {
      setError(err instanceof ApiError && (err.status === 401 || err.status === 403) ? 'Admin Key 不正确' : errorMessage(err))
    } finally {
      setPending(false)
    }
  }

  return (
    <div className="relative flex min-h-svh items-center justify-center overflow-hidden bg-background p-6">
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0 bg-[radial-gradient(60rem_30rem_at_50%_-10%,color-mix(in_oklch,var(--primary)_18%,transparent),transparent)]"
      />
      <form onSubmit={submit} className="relative w-full max-w-sm space-y-6 rounded-2xl border bg-card p-8 shadow-sm">
        <div className="space-y-2">
          <span className="flex size-10 items-center justify-center rounded-xl bg-primary text-primary-foreground">
            <KeyRound className="size-5" />
          </span>
          <h1 className="text-lg font-semibold">Kiro Console</h1>
          <p className="text-sm text-muted-foreground">输入 Admin API Key 进入账号池网关运维控制台</p>
        </div>
        <div className="space-y-2">
          <Label htmlFor="admin-key">Admin API Key</Label>
          <Input
            id="admin-key"
            type="password"
            autoComplete="current-password"
            autoFocus
            value={key}
            onChange={(e) => setKey(e.target.value)}
            aria-invalid={!!error}
            aria-describedby={error ? 'login-error' : undefined}
          />
          {error && (
            <p id="login-error" className="text-xs text-danger">
              {error}
            </p>
          )}
        </div>
        <div className="space-y-1.5">
          <div className="flex items-center gap-2">
            <Checkbox id="remember" checked={remember} onCheckedChange={(v) => setRemember(v === true)} />
            <Label htmlFor="remember" className="text-sm font-normal">
              在此设备记住
            </Label>
          </div>
          {remember && (
            <p className="flex items-start gap-1.5 text-xs text-warning">
              <ShieldAlert className="mt-0.5 size-3.5 shrink-0" />
              Key 会保存在浏览器本地存储，仅建议在个人设备上勾选。
            </p>
          )}
        </div>
        <Button type="submit" className="w-full" disabled={pending || !key.trim()}>
          {pending && <Loader2 className="animate-spin" />}
          进入控制台
        </Button>
      </form>
    </div>
  )
}
