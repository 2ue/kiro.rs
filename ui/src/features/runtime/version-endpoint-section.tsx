import { useEffect, useState } from 'react'
import { ArrowDown, ArrowUp, Check, Pin, RefreshCw, RotateCcw, X } from 'lucide-react'
import { toast } from 'sonner'
import { Button, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch } from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import {
  useApplyKiroClientVersion,
  useCheckKiroClientVersion,
  usePinKiroClientVersion,
  useRollbackKiroClientVersion,
} from '@/hooks/use-credentials'
import type { KiroVersionLifecycleSnapshot, RuntimeConfig } from '@/types/api'

const OAUTH_ENDPOINTS = [
  { value: 'codewhisperer', label: 'CodeWhisperer' },
  { value: 'amazonq', label: 'Amazon Q' },
  { value: 'runtime', label: 'Kiro Runtime' },
] as const

function sourceLabel(source: string): string {
  if (source === 'pinned') return '固定'
  if (source === 'applied' || source === 'auto' || source === 'auto_applied') return '已应用'
  return '配置'
}

function formatCheckedAt(value?: string | null): string {
  if (!value) return '尚未检查'
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString()
}

export function VersionEndpointSection({
  draft,
  setConfig,
  onLifecycle,
}: {
  draft: RuntimeConfig
  setConfig: <K extends keyof RuntimeConfig>(key: K) => (value: RuntimeConfig[K]) => void
  onLifecycle: (snapshot: KiroVersionLifecycleSnapshot) => void
}) {
  const lifecycle = draft.kiroVersionLifecycle
  const [pinVersion, setPinVersion] = useState(lifecycle.activeVersion)
  const [endpointToAdd, setEndpointToAdd] = useState('')
  const check = useCheckKiroClientVersion()
  const apply = useApplyKiroClientVersion()
  const pin = usePinKiroClientVersion()
  const rollback = useRollbackKiroClientVersion()
  const busy = check.isPending || apply.isPending || pin.isPending || rollback.isPending

  useEffect(() => {
    setPinVersion(lifecycle.activeVersion)
  }, [lifecycle.activeVersion])

  const run = async (
    action: () => Promise<KiroVersionLifecycleSnapshot>,
    success: string,
  ) => {
    try {
      const result = await action()
      onLifecycle(result)
      toast.success(success)
    } catch (error) {
      toast.error(`版本操作失败: ${extractErrorMessage(error)}`)
    }
  }

  const candidate = lifecycle.candidateVersion
  const canApply = Boolean(candidate && candidate !== lifecycle.activeVersion)

  return (
    <div className="space-y-5">
      <section className="space-y-3 rounded-lg border bg-muted/10 p-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="text-sm font-semibold">Kiro 客户端版本生命周期</div>
            <div className="mt-1 max-w-3xl text-xs leading-5 text-muted-foreground">
              自动检查只读取官方版本并保存 candidate，不会自动替换 active。关闭检查也不会回退已应用版本；应用或固定版本后，可用回退按钮返回上一个版本。
            </div>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={() => void run(() => check.mutateAsync(), '已读取官方版本 candidate')}
          >
            <RefreshCw className={check.isPending ? 'animate-spin' : ''} />
            立即检查
          </Button>
        </div>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          <div className="rounded-md border bg-background p-3">
            <div className="text-[11px] text-muted-foreground">当前生效</div>
            <div className="mt-1 font-mono text-sm">{lifecycle.activeVersion}</div>
            <div className="mt-1 text-xs text-muted-foreground">来源：{sourceLabel(lifecycle.activeSource)}</div>
          </div>
          <div className="rounded-md border bg-background p-3">
            <div className="text-[11px] text-muted-foreground">配置版本</div>
            <div className="mt-1 font-mono text-sm">{lifecycle.configuredVersion}</div>
          </div>
          <div className="rounded-md border bg-background p-3">
            <div className="text-[11px] text-muted-foreground">candidate</div>
            <div className="mt-1 font-mono text-sm">{candidate || '-'}</div>
            <div className="mt-1 text-xs text-muted-foreground">检查：{formatCheckedAt(lifecycle.checkedAt)}</div>
          </div>
          <div className="rounded-md border bg-background p-3">
            <div className="text-[11px] text-muted-foreground">上一个版本</div>
            <div className="mt-1 font-mono text-sm">{lifecycle.previousVersion || '-'}</div>
            {lifecycle.lastError && <div className="mt-1 truncate text-xs text-destructive" title={lifecycle.lastError}>{lifecycle.lastError}</div>}
          </div>
        </div>
        <div className="flex flex-wrap items-end gap-2">
          <Button
            size="sm"
            disabled={!canApply || busy}
            onClick={() => void run(() => apply.mutateAsync(undefined), `已应用 ${candidate}`)}
          >
            <Check />
            应用 candidate
          </Button>
          <div className="flex min-w-[15rem] flex-1 items-center gap-2 sm:flex-none">
            <Input
              aria-label="固定 Kiro 客户端版本"
              className="h-8 font-mono text-xs"
              value={pinVersion}
              onChange={(event) => setPinVersion(event.target.value)}
              placeholder="例如 0.11.107"
            />
            <Button
              size="sm"
              variant="outline"
              disabled={!pinVersion.trim() || busy}
              onClick={() => void run(() => pin.mutateAsync(pinVersion.trim()), `已固定 ${pinVersion.trim()}`)}
            >
              <Pin />
              固定
            </Button>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={!lifecycle.previousVersion || busy}
            onClick={() => void run(() => rollback.mutateAsync(), '已回退到上一个版本')}
          >
            <RotateCcw />
            回退
          </Button>
        </div>
        <div className="flex items-center justify-between gap-4 border-t pt-3">
          <div>
            <div className="text-sm font-semibold">自动检查版本</div>
            <div className="mt-1 text-xs leading-5 text-muted-foreground">每 12 小时发现 candidate。保存后后台生效，active 仍需显式应用。</div>
          </div>
          <Switch checked={draft.kiroVersionAutoSyncEnabled} onCheckedChange={setConfig('kiroVersionAutoSyncEnabled')} />
        </div>
      </section>

      <section className="space-y-4 rounded-lg border bg-muted/10 p-4">
        <div>
          <div className="text-sm font-semibold">协议端点 fallback</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">
            仅在确认是 endpoint 或 host 故障、且候选端点支持当前认证类型时，在同一账号内尝试下一端点。账号鉴权、额度、风控和普通 429 仍交给账号调度。
          </div>
        </div>
        <div className="flex items-start justify-between gap-4">
          <div>
            <div className="text-sm font-semibold">启用同账号端点 fallback</div>
            <div className="mt-1 text-xs leading-5 text-muted-foreground">关闭时保持账号原端点行为。当前支持的候选由服务端 registry 校验。</div>
          </div>
          <Switch checked={draft.kiroEndpointFallbackEnabled} onCheckedChange={setConfig('kiroEndpointFallbackEnabled')} />
        </div>
        <div className="space-y-2">
          <div className="text-sm font-semibold">候选端点顺序</div>
          <div className="space-y-1.5">
            {draft.kiroEndpointFallbackOrder.map((endpoint, index) => (
              <div key={endpoint} className="flex items-center gap-2 rounded-md border bg-background px-2 py-1.5">
                <span className="w-6 text-center text-xs tabular-nums text-muted-foreground">{index + 1}</span>
                <span className="min-w-0 flex-1 text-sm">{OAUTH_ENDPOINTS.find((item) => item.value === endpoint)?.label ?? endpoint}</span>
                <Button size="icon-xs" variant="ghost" title="上移" disabled={!draft.kiroEndpointFallbackEnabled || index === 0} onClick={() => {
                  const order = [...draft.kiroEndpointFallbackOrder]
                  ;[order[index - 1], order[index]] = [order[index]!, order[index - 1]!]
                  setConfig('kiroEndpointFallbackOrder')(order)
                }}><ArrowUp /></Button>
                <Button size="icon-xs" variant="ghost" title="下移" disabled={!draft.kiroEndpointFallbackEnabled || index === draft.kiroEndpointFallbackOrder.length - 1} onClick={() => {
                  const order = [...draft.kiroEndpointFallbackOrder]
                  ;[order[index], order[index + 1]] = [order[index + 1]!, order[index]!]
                  setConfig('kiroEndpointFallbackOrder')(order)
                }}><ArrowDown /></Button>
                <Button size="icon-xs" variant="ghost" title="移除" disabled={!draft.kiroEndpointFallbackEnabled} onClick={() => setConfig('kiroEndpointFallbackOrder')(draft.kiroEndpointFallbackOrder.filter((name) => name !== endpoint))}><X /></Button>
              </div>
            ))}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Select value={endpointToAdd} onValueChange={setEndpointToAdd} disabled={!draft.kiroEndpointFallbackEnabled || draft.kiroEndpointFallbackOrder.length >= OAUTH_ENDPOINTS.length}>
              <SelectTrigger className="h-8 w-52"><SelectValue placeholder="添加 OAuth 端点" /></SelectTrigger>
              <SelectContent>
                {OAUTH_ENDPOINTS.filter((item) => !draft.kiroEndpointFallbackOrder.includes(item.value)).map((item) => <SelectItem key={item.value} value={item.value}>{item.label} · {item.value}</SelectItem>)}
              </SelectContent>
            </Select>
            <Button size="sm" variant="outline" disabled={!draft.kiroEndpointFallbackEnabled || !endpointToAdd} onClick={() => {
              if (draft.kiroEndpointFallbackOrder.includes(endpointToAdd)) return
              setConfig('kiroEndpointFallbackOrder')([...draft.kiroEndpointFallbackOrder, endpointToAdd])
              setEndpointToAdd('')
            }}>添加</Button>
          </div>
          <div className="text-xs leading-5 text-muted-foreground">仅支持 OAuth 兼容端点：codewhisperer、amazonq、runtime。primary 仍按凭据 endpoint 优先、再按 defaultEndpoint 解析；API Key 账号不参与这三个候选。</div>
        </div>
      </section>
    </div>
  )
}
