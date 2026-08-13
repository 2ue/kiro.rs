import * as React from 'react'
import { Activity, Database, KeyRound, Route, ShieldCheck } from 'lucide-react'
import { storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/http'
import { extractErrorMessage } from '@/lib/utils'
import { Button, Input } from '@/components/ui'
import { Callout } from '@/components/patterns'

const signalCards = [
  { title: '状态', desc: '查看关键指标', icon: Activity },
  { title: '资源', desc: '维护后台资源', icon: Database },
  { title: '设置', desc: '调整运行配置', icon: Route },
]

export function LoginPage({
  initialError = '',
  onLogin,
}: {
  initialError?: string
  onLogin: () => void
}) {
  const [adminApiKey, setAdminApiKey] = React.useState(storage.getApiKey() || '')
  const [error, setError] = React.useState(initialError)
  const [submitting, setSubmitting] = React.useState(false)

  React.useEffect(() => {
    setError(initialError)
  }, [initialError])

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    const trimmed = adminApiKey.trim()
    if (!trimmed) {
      setError('请输入登录 Key')
      return
    }
    setSubmitting(true)
    setError('')
    try {
      await validateAdminApiKey(trimmed)
      storage.setApiKey(trimmed)
      onLogin()
    } catch (err) {
      storage.removeApiKey()
      setError(extractErrorMessage(err))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <main className="min-h-screen bg-background">
      <div className="absolute inset-x-0 top-0 h-0.5 bg-primary" />
      <div className="mx-auto flex min-h-screen w-full max-w-5xl items-center justify-center px-4 py-8">
        <div className="grid w-full overflow-hidden rounded-2xl bg-card shadow-xl lg:grid-cols-[1fr_410px]">
          {/* 左侧品牌区 */}
          <section className="hidden bg-muted/40 p-8 lg:block">
            <div className="flex h-full flex-col justify-between">
              <div>
                <div className="flex items-center gap-3">
                  <div className="flex size-11 items-center justify-center rounded-xl bg-secondary text-primary">
                    <ShieldCheck className="size-5" />
                  </div>
                  <div>
                    <div className="text-xl font-bold tracking-tight">Kiro Console</div>
                    <div className="text-[0.72rem] font-semibold text-muted-foreground">
                      管理控制台
                    </div>
                  </div>
                </div>

                <h1 className="mt-12 max-w-md text-4xl font-semibold leading-tight text-foreground">
                  管理控制台
                  <br />
                  <span className="text-primary">清晰、稳定、可控</span>
                </h1>
                <p className="mt-4 max-w-sm text-sm leading-6 text-muted-foreground">
                  用于查看状态、维护资源和调整设置。
                </p>
              </div>

              <div className="grid grid-cols-3 gap-3">
                {signalCards.map((feature) => {
                  const Icon = feature.icon
                  return (
                    <div
                      key={feature.title}
                      className="rounded-xl bg-background/70 p-3.5 shadow-sm"
                    >
                      <Icon className="mb-3 size-4 text-primary" />
                      <div className="text-sm font-semibold">{feature.title}</div>
                      <div className="text-[0.68rem] text-muted-foreground">{feature.desc}</div>
                    </div>
                  )
                })}
              </div>
            </div>
          </section>

          {/* 右侧表单区 */}
          <section className="p-6 sm:p-8">
            <div className="mb-8 flex items-center gap-3 lg:hidden">
              <div className="flex size-10 items-center justify-center rounded-xl bg-secondary text-primary">
                <ShieldCheck className="size-5" />
              </div>
              <span className="text-xl font-bold">Kiro Console</span>
            </div>

            <div className="mb-8">
              <h2 className="text-2xl font-semibold tracking-tight">控制台登录</h2>
              <p className="mt-2 text-sm leading-6 text-muted-foreground">
                输入登录 Key 进入管理控制台
              </p>
            </div>

            <form className="space-y-5" onSubmit={handleSubmit}>
              <div className="space-y-2">
                <label className="text-sm font-medium text-foreground/80">登录 Key</label>
                <div className="relative">
                  <KeyRound className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    type="password"
                    className="pl-9"
                    value={adminApiKey}
                    onChange={(event) => {
                      setAdminApiKey(event.target.value)
                      setError('')
                    }}
                    placeholder="请输入登录 Key"
                    autoFocus
                  />
                </div>
                <p className="text-xs leading-5 text-muted-foreground">
                  请使用管理员提供的后台登录 Key。
                </p>
              </div>

              {error && <Callout tone="error">{error}</Callout>}

              <Button type="submit" className="w-full" disabled={submitting || !adminApiKey.trim()}>
                {submitting ? '验证中...' : '进入控制台'}
              </Button>
            </form>

            <div className="mt-8 text-center text-xs text-muted-foreground/70">本地管理控制台</div>
          </section>
        </div>
      </div>
    </main>
  )
}
