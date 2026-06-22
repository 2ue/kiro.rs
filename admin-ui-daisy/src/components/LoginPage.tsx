import { Activity, Command, Database, KeyRound, Route, ShieldCheck } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Alert, Button, Form, Input, Join } from 'react-daisyui'
import { storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'

const signalCards = [
  { title: '查看状态', desc: '掌握整体情况', icon: <Activity className="h-4 w-4" /> },
  { title: '管理资源', desc: '维护后台资源', icon: <Database className="h-4 w-4" /> },
  { title: '调整设置', desc: '统一配置入口', icon: <Route className="h-4 w-4" /> },
]

export function LoginPage({ initialError = '', onLogin }: { initialError?: string; onLogin: () => void }) {
  const [adminApiKey, setAdminApiKey] = useState(storage.getApiKey() || '')
  const [error, setError] = useState(initialError)
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
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
    } catch (error) {
      storage.removeApiKey()
      setError(extractErrorMessage(error))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <main className="auth-shell min-h-screen">
      <div className="mx-auto flex min-h-screen w-full max-w-6xl items-center justify-center px-4 py-8">
        <div className="auth-card grid w-full overflow-hidden rounded-box lg:grid-cols-[1.08fr_420px]">
          {/* Left Panel - Branding */}
          <section className="auth-visual hidden border-r border-primary/15 p-8 lg:block">
            <div className="flex h-full flex-col justify-between">
              <div>
                <div className="flex items-center gap-3">
                  <div className="brand-mark flex h-10 w-10 items-center justify-center rounded-xl">
                    <Command className="h-5 w-5" />
                  </div>
                  <span>
                    <span className="block text-xl font-bold tracking-tight">Kiro Admin</span>
                    <span className="block text-[0.68rem] font-semibold uppercase text-primary/80">Admin Console</span>
                  </span>
                </div>

                <h1 className="mt-12 text-4xl font-bold leading-tight text-base-content">
                  管理控制台
                  <br />
                  <span className="text-primary">简洁、安全、集中</span>
                </h1>

                <p className="mt-4 max-w-sm text-sm leading-relaxed text-base-content/55">
                  用于查看状态、管理资源和调整设置。
                </p>
              </div>

              <div className="space-y-4">
                <div className="grid grid-cols-3 gap-3">
                  {signalCards.map((feature) => (
                    <div key={feature.title} className="signal-card rounded-lg p-3">
                      <div className="mb-3 text-primary">{feature.icon}</div>
                      <div className="text-sm font-semibold">{feature.title}</div>
                      <div className="text-[0.68rem] text-base-content/45">{feature.desc}</div>
                    </div>
                  ))}
                </div>
                <div className="glass-panel relative overflow-hidden rounded-box p-4">
                  <div className="mb-4 flex items-center justify-between gap-3">
                    <div>
                      <div className="text-xs font-semibold uppercase text-base-content/45">安全入口</div>
                      <div className="mt-1 text-lg font-bold">登录凭证</div>
                    </div>
                    <ShieldCheck className="h-6 w-6 text-primary" />
                  </div>
                  <div className="space-y-2">
                    <div className="signal-line w-11/12" />
                    <div className="signal-line signal-line-muted w-8/12" />
                    <div className="signal-line w-10/12" />
                  </div>
                </div>
              </div>
            </div>
          </section>

          {/* Right Panel - Login Form */}
          <section className="bg-base-100/35 p-6 sm:p-8">
            {/* Mobile Logo */}
            <div className="mb-8 flex items-center gap-3 lg:hidden">
              <div className="brand-mark flex h-10 w-10 items-center justify-center rounded-xl">
                <Command className="h-5 w-5" />
              </div>
              <span className="text-xl font-bold">Kiro Admin</span>
            </div>

            <div className="mb-8">
              <h2 className="text-2xl font-bold tracking-tight">控制台登录</h2>
              <p className="mt-2 text-sm text-base-content/60">
                输入登录 Key 进入管理控制台
              </p>
            </div>

            <Form className="space-y-5" onSubmit={handleSubmit}>
              <div className="space-y-2">
                <label className="text-sm font-medium text-base-content/70">
                  登录 Key
                </label>
                <Join className="w-full">
                  <Button
                    type="button"
                    className="join-item cursor-default border-primary/20 bg-base-200/70"
                  >
                    <KeyRound className="h-4 w-4 text-base-content/50" />
                  </Button>
                  <Input
                    bordered
                    type="password"
                    className="join-item w-full"
                    value={adminApiKey}
                    onChange={(event) => {
                      setAdminApiKey(event.target.value)
                      setError('')
                    }}
                    placeholder="请输入登录 Key"
                  />
                </Join>
                <p className="text-xs leading-5 text-base-content/50">
                  请使用管理员提供的后台登录凭证。
                </p>
              </div>

              {error && (
                <Alert status="error" className="py-2.5 text-sm">
                  {error}
                </Alert>
              )}

              <Button
                type="submit"
                color="primary"
                className="w-full"
                disabled={submitting || !adminApiKey.trim()}
              >
                {submitting ? '验证中...' : '进入控制台'}
              </Button>
            </Form>

            <div className="mt-8 text-center text-xs text-base-content/40">
              Kiro Admin Console
            </div>
          </section>
        </div>
      </div>
    </main>
  )
}
