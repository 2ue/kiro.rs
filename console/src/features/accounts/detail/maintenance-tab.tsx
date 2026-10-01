import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Eraser, FlaskConical, KeyRound, Loader2, RotateCcw, Trash2, Zap } from 'lucide-react'
import { toast } from 'sonner'
import { credentialsApi } from '@/api/endpoints/credentials'
import type { AddCredentialRequest } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { SelectControl } from '@/components/patterns/fields'
import { qk } from '@/queries/keys'
import { useModelCapabilities } from '@/queries/shared'
import { fmtMs } from '@/lib/format'
import { useAccountActions } from '../actions'
import type { AccountRow } from '../queries'

const FALLBACK_TEST_MODELS = ['claude-sonnet-4.5', 'claude-haiku-4.5', 'claude-opus-4.5', 'claude-sonnet-4.6', 'claude-opus-4.6']

function ActionRow({ icon, title, description, children }: { icon: React.ReactNode; title: string; description: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-3 py-3">
      <span className="mt-0.5 text-muted-foreground [&_svg]:size-4">{icon}</span>
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{title}</div>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  )
}

export function MaintenanceTab({ row, onDeleted }: { row: AccountRow; onDeleted: () => void }) {
  const actions = useAccountActions()
  const queryClient = useQueryClient()
  const capabilities = useModelCapabilities()
  const invalidate = () => queryClient.invalidateQueries({ queryKey: qk.credentials.all })

  const modelOptions = [...new Set([...(row.supportedModels ?? []), ...(capabilities.data?.models.map((m) => m.model) ?? []), ...FALLBACK_TEST_MODELS])]
  const [testModel, setTestModel] = useState(modelOptions.find((m) => m !== 'auto') ?? FALLBACK_TEST_MODELS[0]!)
  const [testPrompt, setTestPrompt] = useState('hi')
  const test = useMutation({
    mutationFn: () => credentialsApi.test(row.id, { model: testModel, prompt: testPrompt }),
    meta: { error: '测试失败' },
  })
  const clearInFlight = useMutation({
    mutationFn: () => credentialsApi.clearInFlight(row.id),
    onSettled: invalidate,
    meta: { success: '已清理卡住的在途占用', error: '清理失败' },
  })
  const overage = useMutation({
    mutationFn: (enabled: boolean) => credentialsApi.setOverage(row.id, enabled),
    onSettled: invalidate,
    meta: { success: '超额设置已更新', error: '更新超额失败' },
  })

  const [auth, setAuth] = useState<Partial<AddCredentialRequest>>({})
  const [resetRuntime, setResetRuntime] = useState(true)
  const updateAuth = useMutation({
    mutationFn: () => credentialsApi.updateAuth(row.id, { ...stripEmpty(auth), resetRuntimeState: resetRuntime }),
    onSuccess: () => {
      toast.success('认证信息已更新')
      setAuth({})
    },
    onSettled: invalidate,
    meta: { error: '更新认证失败' },
  })
  const isApiKey = row.authMethod === 'api_key'

  return (
    <div className="space-y-6">
      <section>
        <h3 className="mb-1 text-sm font-medium">连通性测试</h3>
        <p className="mb-3 text-xs text-muted-foreground">用该账号向上游发送一次真实请求，会消耗少量额度。</p>
        <div className="grid gap-2 sm:grid-cols-[1fr_1fr_auto]">
          <SelectControl value={testModel} onChange={setTestModel} options={modelOptions.map((m) => ({ value: m, label: m }))} />
          <Input value={testPrompt} onChange={(e) => setTestPrompt(e.target.value)} aria-label="测试提示词" />
          <Button size="sm" onClick={() => test.mutate()} disabled={test.isPending}>
            {test.isPending ? <Loader2 className="animate-spin" /> : <FlaskConical />} 测试
          </Button>
        </div>
        {test.data && (
          <div className="mt-3 rounded-lg border bg-success-subtle/40 p-3 text-xs">
            <div className="font-medium text-success">
              成功 · {test.data.modelId} · {fmtMs(test.data.durationMs)}
            </div>
            <p className="mt-1 line-clamp-4 whitespace-pre-wrap text-muted-foreground">{test.data.response}</p>
          </div>
        )}
      </section>

      <section className="divide-y border-y">
        <ActionRow icon={<RotateCcw />} title="重置失败计数" description="清零失败与刷新失败次数，账号会重新参与调度">
          <Button size="sm" variant="outline" onClick={() => actions.resetFailures.mutate([row.id])}>
            重置
          </Button>
        </ActionRow>
        <ActionRow icon={<KeyRound />} title="强制刷新 Token" description="立即向认证服务刷新 Access Token">
          <Button size="sm" variant="outline" onClick={() => actions.refreshToken.mutate(row.id)} disabled={actions.refreshToken.isPending}>
            刷新
          </Button>
        </ActionRow>
        <ActionRow icon={<Eraser />} title="清理在途占用" description="回收长时间未结束的并发占用；仅在并发数异常卡住时使用">
          <Button size="sm" variant="outline" onClick={() => clearInFlight.mutate()} disabled={clearInFlight.isPending || row.inFlightRequests === 0}>
            清理
          </Button>
        </ActionRow>
        <ActionRow icon={<Zap />} title="超额计费" description="开启后额度用尽仍可继续使用，按超额费率计费">
          <Switch
            checked={row.quota.overageEnabled}
            disabled={overage.isPending}
            onCheckedChange={(v) => overage.mutate(v)}
            aria-label="超额计费"
          />
        </ActionRow>
      </section>

      <section>
        <h3 className="mb-1 text-sm font-medium">更新认证信息</h3>
        <p className="mb-3 text-xs text-muted-foreground">只提交填写的字段；适用于账号重新登录后替换凭据。</p>
        <div className="grid gap-2">
          {isApiKey ? (
            <Input type="password" placeholder="新的 Kiro API Key (ksk_…)" value={auth.kiroApiKey ?? ''} onChange={(e) => setAuth((a) => ({ ...a, kiroApiKey: e.target.value }))} />
          ) : (
            <>
              <Textarea rows={2} placeholder="新的 Refresh Token" className="font-mono text-xs" value={auth.refreshToken ?? ''} onChange={(e) => setAuth((a) => ({ ...a, refreshToken: e.target.value }))} />
              {row.authMethod === 'idc' && (
                <div className="grid gap-2 sm:grid-cols-2">
                  <Input placeholder="clientId" value={auth.clientId ?? ''} onChange={(e) => setAuth((a) => ({ ...a, clientId: e.target.value }))} />
                  <Input type="password" placeholder="clientSecret" value={auth.clientSecret ?? ''} onChange={(e) => setAuth((a) => ({ ...a, clientSecret: e.target.value }))} />
                </div>
              )}
            </>
          )}
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <Switch id="reset-runtime" checked={resetRuntime} onCheckedChange={setResetRuntime} />
              <Label htmlFor="reset-runtime" className="text-xs font-normal">
                同时清除冷却与失败计数
              </Label>
            </div>
            <Button size="sm" onClick={() => updateAuth.mutate()} disabled={updateAuth.isPending || Object.keys(stripEmpty(auth)).length === 0}>
              更新
            </Button>
          </div>
        </div>
      </section>

      <section className="rounded-lg border border-danger/30 p-3">
        <ActionRow icon={<Trash2 className="text-danger" />} title="删除账号" description="删除凭据与运行态，历史请求记录保留">
          <Button
            size="sm"
            variant="destructive"
            onClick={async () => {
              if (await actions.confirmRemove([row.id], row.label)) onDeleted()
            }}
          >
            删除
          </Button>
        </ActionRow>
      </section>
    </div>
  )
}

function stripEmpty<T extends object>(obj: T): Partial<T> {
  return Object.fromEntries(Object.entries(obj).filter(([, v]) => typeof v === 'string' ? v.trim() !== '' : v !== undefined)) as Partial<T>
}
