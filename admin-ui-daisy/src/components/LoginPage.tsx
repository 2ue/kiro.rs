import { KeyRound, Server } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Alert, Button, Card, Form, Input, Join } from 'react-daisyui'
import { storage } from '@/lib/storage'
import { validateAdminApiKey } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'

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
    <main className="min-h-screen bg-base-200">
      <div className="mx-auto flex min-h-screen w-full max-w-6xl items-center justify-center px-4 py-8">
        <Card className="grid w-full overflow-hidden border border-base-300 bg-base-100 shadow-xl lg:grid-cols-[1fr_420px]">
          <section className="hidden bg-neutral p-10 text-neutral-content lg:block">
            <div className="flex h-full flex-col justify-between">
              <div>
                <div className="flex items-center gap-3 text-xl font-semibold">
                  <Server className="h-6 w-6" />
                  Kiro Admin
                </div>
                <h1 className="mt-14 max-w-lg text-4xl font-semibold leading-tight">
                  凭据、缓存、计费与审计的统一后台
                </h1>
                <p className="mt-4 max-w-xl text-sm leading-7 text-neutral-content/70">
                  新版前端使用 React、TypeScript、Tailwind CSS 和 DaisyUI 独立实现，能力对齐旧后台，交互更集中。
                </p>
              </div>
              <div className="grid grid-cols-3 gap-3 text-sm">
                <div className="rounded-box bg-neutral-content/10 p-3">凭据调度</div>
                <div className="rounded-box bg-neutral-content/10 p-3">Usage 追踪</div>
                <div className="rounded-box bg-neutral-content/10 p-3">运行配置</div>
              </div>
            </div>
          </section>
          <section className="p-6 sm:p-10">
            <div className="mb-8 flex items-center gap-3 lg:hidden">
              <Server className="h-6 w-6" />
              <span className="text-xl font-semibold">Kiro Admin</span>
            </div>
            <div className="mb-8">
              <h2 className="text-2xl font-semibold">登录后台</h2>
              <p className="mt-2 text-sm text-base-content/60">输入配置中的 admin API Key 后进入管理页面。</p>
            </div>
            <Form className="space-y-4" onSubmit={handleSubmit}>
              <Form.Label title="后台 API Key">
                <Join className="w-full">
                  <Button type="button" className="join-item cursor-default border-base-300 bg-base-200">
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
              </Form.Label>
              {error && <Alert status="error" className="py-2 text-sm">{error}</Alert>}
              <Button type="submit" color="primary" className="w-full" disabled={submitting || !apiKey.trim()}>
                {submitting ? '验证中...' : '进入后台'}
              </Button>
            </Form>
          </section>
        </Card>
      </div>
    </main>
  )
}
