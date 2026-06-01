import { Command, KeyRound, Sparkles } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Alert, Button, Card, Form, Input, Join } from 'react-daisyui'
import { storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'

const features = [
  { title: '凭据调度', desc: '智能负载均衡和故障转移' },
  { title: 'Usage 追踪', desc: '实时用量统计和费用估算' },
  { title: '运行配置', desc: '热加载的运行时参数' },
]

export function LoginPage({ initialError = '', onLogin }: { initialError?: string; onLogin: () => void }) {
  const [apiKey, setApiKey] = useState(storage.getApiKey() || '')
  const [error, setError] = useState(initialError)
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
    setError(initialError)
  }, [initialError])

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    const trimmed = apiKey.trim()
    if (!trimmed) {
      setError('请输入后台 API Key')
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
    <main className="min-h-screen bg-gradient-to-br from-base-200 via-base-100 to-base-200">
      <div className="mx-auto flex min-h-screen w-full max-w-5xl items-center justify-center px-4 py-8">
        <Card className="grid w-full overflow-hidden border border-base-300/50 bg-base-100 shadow-2xl lg:grid-cols-[1fr_400px]">
          {/* Left Panel - Branding */}
          <section className="relative hidden overflow-hidden bg-gradient-to-br from-primary via-primary/90 to-secondary p-8 text-primary-content lg:block">
            <div className="absolute -right-20 -top-20 h-64 w-64 rounded-full bg-white/5" />
            <div className="absolute -bottom-32 -left-32 h-96 w-96 rounded-full bg-white/5" />

            <div className="relative flex h-full flex-col justify-between">
              <div>
                <div className="flex items-center gap-3">
                  <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-white/10 backdrop-blur">
                    <Command className="h-5 w-5" />
                  </div>
                  <span className="text-xl font-bold tracking-tight">Kiro Admin</span>
                </div>

                <h1 className="mt-12 text-3xl font-bold leading-tight">
                  凭据、缓存、计费
                  <br />
                  <span className="text-white/80">统一管理后台</span>
                </h1>

                <p className="mt-4 max-w-sm text-sm leading-relaxed text-white/70">
                  新版控制台采用现代化设计，提供更直观的凭据管理、实时监控和配置能力。
                </p>
              </div>

              <div className="space-y-3">
                {features.map((feature) => (
                  <div
                    key={feature.title}
                    className="flex items-center gap-3 rounded-xl bg-white/10 p-3 backdrop-blur"
                  >
                    <Sparkles className="h-4 w-4 shrink-0" />
                    <div>
                      <div className="text-sm font-semibold">{feature.title}</div>
                      <div className="text-xs text-white/60">{feature.desc}</div>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          </section>

          {/* Right Panel - Login Form */}
          <section className="p-6 sm:p-8">
            {/* Mobile Logo */}
            <div className="mb-8 flex items-center gap-3 lg:hidden">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-gradient-to-br from-primary to-secondary text-primary-content">
                <Command className="h-5 w-5" />
              </div>
              <span className="text-xl font-bold">Kiro Admin</span>
            </div>

            <div className="mb-8">
              <h2 className="text-2xl font-bold tracking-tight">欢迎回来</h2>
              <p className="mt-2 text-sm text-base-content/60">
                输入管理员 API Key 进入控制台
              </p>
            </div>

            <Form className="space-y-5" onSubmit={handleSubmit}>
              <div className="space-y-2">
                <label className="text-sm font-medium text-base-content/70">API Key</label>
                <Join className="w-full">
                  <Button
                    type="button"
                    className="join-item cursor-default border-base-300 bg-base-200"
                  >
                    <KeyRound className="h-4 w-4 text-base-content/50" />
                  </Button>
                  <Input
                    bordered
                    type="password"
                    className="join-item w-full"
                    value={apiKey}
                    onChange={(event) => {
                      setApiKey(event.target.value)
                      setError('')
                    }}
                    placeholder="sk-admin-..."
                  />
                </Join>
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
                disabled={submitting || !apiKey.trim()}
              >
                {submitting ? '验证中...' : '进入控制台'}
              </Button>
            </Form>

            <div className="mt-8 text-center text-xs text-base-content/40">
              Kiro Admin Console v2.0
            </div>
          </section>
        </Card>
      </div>
    </main>
  )
}
