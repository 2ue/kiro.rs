import { useMemo, useState } from 'react'
import { Activity } from 'lucide-react'
import { toast } from 'sonner'
import {
  Badge,
  Button,
  Checkbox,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
} from '@/components/ui'
import { Field, FieldGrid } from '@/components/patterns'
import { SecretInput } from '@/features/credentials/credential-inputs'
import { useCredentials, useTestAllProxyResources } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import { cn } from '@/lib/utils'
import type { ProxyResource } from '@/types/api'
import {
  DEFAULT_AUTO_PICK,
  autoPickProxyIds,
  proxyUsageById,
  rankProxies,
  type ProxyLiveness,
  type ProxyRankMode,
} from './proxy-ranking'

/**
 * How a batch of accounts gets its proxy. Shared by every import and by batch edit so the
 * choices, wording and rotation picker are the same everywhere.
 */
export type ProxyAssignmentMode = 'single' | 'round_robin' | 'direct_url' | 'none' | 'clear'

export interface ProxyAssignmentValue {
  mode: ProxyAssignmentMode
  /** `single`: the one proxy resource. */
  resourceId: string
  /** `round_robin`: proxy resources in rotation order. */
  resourceIds: string[]
  /** `direct_url`: a proxy written into every account. */
  url: string
  username: string
  password: string
}

export function initialProxyAssignment(mode: ProxyAssignmentMode = 'single'): ProxyAssignmentValue {
  return { mode, resourceId: '', resourceIds: [], url: '', username: '', password: '' }
}

/** A message when the value cannot be applied yet, otherwise undefined. */
export function validateProxyAssignmentValue(value: ProxyAssignmentValue, options: { requireSingle?: boolean } = {}): string | undefined {
  if (value.mode === 'single' && options.requireSingle && !value.resourceId) return '请选择代理资源，或改为"不使用代理"'
  if (value.mode === 'round_robin' && value.resourceIds.length === 0) return '已选择多个代理轮换，请至少选择一个代理资源'
  if (value.mode === 'direct_url' && !value.url.trim()) return '请填写代理 URL'
  return undefined
}

const MODE_LABEL: Record<ProxyAssignmentMode, string> = {
  single: '绑定一个代理资源',
  round_robin: '多个代理资源轮换',
  direct_url: '直接填写代理 URL',
  none: '不使用代理',
  clear: '清除账号代理',
}

const RANK_MODE_LABEL: Record<ProxyRankMode, string> = {
  usage: '使用优先',
  quality: '质量优先',
}

function latencyTone(result: ProxyLiveness): 'success' | 'info' | 'warning' | 'error' {
  if (!result.success) return 'error'
  if (result.durationMs < 200) return 'success'
  if (result.durationMs <= 500) return 'info'
  if (result.durationMs <= 1000) return 'warning'
  return 'error'
}

/** The test result a proxy already has on record, so a ranking can use it before a fresh test. */
function storedLiveness(resource: ProxyResource): ProxyLiveness | undefined {
  if (resource.lastTestSuccess === null || resource.lastTestSuccess === undefined) return undefined
  return {
    success: resource.lastTestSuccess,
    durationMs: resource.lastTestDurationMs ?? 0,
    message: resource.lastTestMessage ?? undefined,
  }
}

export function ProxyAssignmentField({
  value,
  onChange,
  proxyResources,
  modes,
  disabled,
  description,
}: {
  value: ProxyAssignmentValue
  onChange: (value: ProxyAssignmentValue) => void
  /** Enabled proxy resources the user may pick from. */
  proxyResources: ProxyResource[]
  /** Which choices this form offers, in display order. */
  modes: ProxyAssignmentMode[]
  disabled?: boolean
  description?: string
}) {
  const [showSecret, setShowSecret] = useState(false)
  const [rankMode, setRankMode] = useState<ProxyRankMode>('usage')
  const [pickCount, setPickCount] = useState(String(DEFAULT_AUTO_PICK))
  const [freshLiveness, setFreshLiveness] = useState<Map<number, ProxyLiveness> | null>(null)
  /** The current selection came from "pick the top N"; a hand-edited one is left alone on re-sort. */
  const [autoPicked, setAutoPicked] = useState(false)
  const testAll = useTestAllProxyResources()
  // Bindings decide the usage order, so load them as soon as rotation is on screen.
  const credentials = useCredentials({ enabled: value.mode === 'round_robin', refetchInterval: false, staleTime: 30_000 })

  const liveness = useMemo(() => {
    if (freshLiveness) return freshLiveness
    const stored = new Map<number, ProxyLiveness>()
    for (const resource of proxyResources) {
      const result = storedLiveness(resource)
      if (result) stored.set(resource.id, result)
    }
    return stored
  }, [freshLiveness, proxyResources])
  const usage = useMemo(() => proxyUsageById(credentials.data?.credentials ?? []), [credentials.data?.credentials])
  const ranked = useMemo(
    () => rankProxies(proxyResources, liveness, usage, rankMode),
    [proxyResources, liveness, usage, rankMode],
  )
  const selected = new Set(value.resourceIds)
  const liveCount = ranked.filter((item) => item.usable && item.liveness?.success).length

  const setMode = (mode: ProxyAssignmentMode) => onChange({ ...initialProxyAssignment(mode), resourceIds: mode === 'round_robin' ? value.resourceIds : [] })

  const pick = (ids: number[], fromRanking = false) => {
    setAutoPicked(fromRanking)
    onChange({ ...value, mode: 'round_robin', resourceIds: ids.map(String) })
  }

  const toggle = (id: number, checked: boolean) => {
    setAutoPicked(false)
    const next = value.resourceIds.filter((item) => item !== String(id))
    // Keep the rotation in rank order so the list reads top to bottom as account 1, 2, 3…
    if (checked) {
      const order = new Map(ranked.map((item, index) => [String(item.resource.id), index]))
      next.push(String(id))
      next.sort((left, right) => (order.get(left) ?? 0) - (order.get(right) ?? 0))
    }
    onChange({ ...value, mode: 'round_robin', resourceIds: next })
  }

  const pickTop = (rankedList = ranked) => {
    const count = Number(pickCount)
    const limit = Number.isInteger(count) && count > 0 ? count : DEFAULT_AUTO_PICK
    const ids = autoPickProxyIds(rankedList, limit)
    pick(ids, true)
    return ids
  }

  const runFullTest = async () => {
    try {
      // Bindings change over time; count them from a fresh snapshot right before ranking.
      const [result, snapshot] = await Promise.all([testAll.mutateAsync({}), credentials.refetch()])
      const next = new Map<number, ProxyLiveness>()
      for (const item of result.items) next.set(item.resourceId, { success: item.success, durationMs: item.durationMs, message: item.message })
      setFreshLiveness(next)
      const nextUsage = proxyUsageById(snapshot.data?.credentials ?? [])
      const ids = pickTop(rankProxies(proxyResources, next, nextUsage, rankMode))
      toast.success(`测活完成：${result.success}/${result.total} 个可用，已按${RANK_MODE_LABEL[rankMode]}选中前 ${ids.length} 个`)
    } catch (error) {
      toast.error(`全量测活失败: ${extractErrorMessage(error)}`)
    }
  }

  const changeRankMode = (mode: ProxyRankMode) => {
    setRankMode(mode)
    // Re-pick only when the current pick came from ranking; a hand-made selection is kept.
    if (autoPicked) pickTop(rankProxies(proxyResources, liveness, usage, mode))
  }

  const busy = disabled || testAll.isPending

  return (
    <div className="space-y-3">
      <Field label="代理分配方式" description={description}>
        <Select value={value.mode} onValueChange={(next) => setMode(next as ProxyAssignmentMode)} disabled={disabled}>
          <SelectTrigger size="sm" className="sm:w-64"><SelectValue /></SelectTrigger>
          <SelectContent>
            {modes.map((mode) => <SelectItem key={mode} value={mode}>{MODE_LABEL[mode]}</SelectItem>)}
          </SelectContent>
        </Select>
      </Field>

      {value.mode === 'single' && (
        <Field label="代理资源">
          <Select value={value.resourceId || '__none__'} onValueChange={(next) => onChange({ ...value, resourceId: next === '__none__' ? '' : next })} disabled={disabled}>
            <SelectTrigger size="sm" className="sm:w-80"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="__none__">选择代理资源</SelectItem>
              {proxyResources.map((resource) => <SelectItem key={resource.id} value={String(resource.id)}>{resource.name}</SelectItem>)}
            </SelectContent>
          </Select>
        </Field>
      )}

      {value.mode === 'round_robin' && (
        <div className="space-y-2">
          <div className="flex flex-wrap items-end gap-2">
            <Button type="button" variant="outline" size="sm" onClick={() => void runFullTest()} disabled={busy || proxyResources.length === 0}>
              {testAll.isPending ? <Spinner size="sm" /> : <Activity />}全量测活并排序
            </Button>
            <label className="flex flex-col gap-1 text-xs text-muted-foreground">
              排序
              <Select value={rankMode} onValueChange={(next) => changeRankMode(next as ProxyRankMode)} disabled={busy}>
                <SelectTrigger size="sm" className="w-32" aria-label="排序方式"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="usage">使用优先</SelectItem>
                  <SelectItem value="quality">质量优先</SelectItem>
                </SelectContent>
              </Select>
            </label>
            <label className="flex flex-col gap-1 text-xs text-muted-foreground">
              自动选前
              <Input type="number" min={1} value={pickCount} onChange={(event) => setPickCount(event.target.value)} disabled={busy} className="h-8 w-20" aria-label="自动选择数量" />
            </label>
            <Button type="button" variant="ghost" size="sm" onClick={() => pickTop()} disabled={busy || liveCount === 0}>按排序选前 {pickCount || DEFAULT_AUTO_PICK} 个</Button>
            <Button type="button" variant="ghost" size="sm" onClick={() => pick([])} disabled={busy || value.resourceIds.length === 0}>清空</Button>
          </div>
          <p className="text-[0.7rem] leading-4 text-muted-foreground">
            {rankMode === 'usage'
              ? '使用优先：没绑账号的在前，其次是只绑了禁用账号的，再按绑定的启用账号从少到多；同一档里速度快的在前。'
              : '质量优先：只按测活速度排序。'}
            测不通的代理排在最后，不能选。
          </p>
          {proxyResources.length > 0 ? (
            // Two columns once the dialog is wide enough; the numbers keep the rank order readable top to bottom.
            <ol className="grid max-h-72 overflow-y-auto rounded-md border bg-background/60 md:grid-flow-col md:grid-cols-2 [&>li]:border-b [&>li]:border-border/60" style={{ gridTemplateRows: `repeat(${Math.ceil(ranked.length / 2)}, auto)` }}>
              {ranked.map((item, index) => {
                const id = item.resource.id
                const checked = selected.has(String(id))
                const position = checked ? value.resourceIds.indexOf(String(id)) + 1 : undefined
                return (
                  <li key={id}>
                    <label className={cn('flex min-w-0 items-center gap-2 px-2 py-1.5 text-sm', item.usable ? 'cursor-pointer hover:bg-muted/60' : 'cursor-not-allowed opacity-50')}>
                      <Checkbox checked={checked} disabled={disabled || !item.usable} onCheckedChange={(next) => toggle(id, next === true)} aria-label={`选择代理 ${item.resource.name}`} />
                      <span className="w-6 shrink-0 text-right font-mono text-[0.7rem] text-muted-foreground">{index + 1}</span>
                      <span className="min-w-0 flex-1 truncate">{item.resource.name}</span>
                      <span className="shrink-0 text-[0.7rem] text-muted-foreground" title={`绑定账号 ${item.usage.total} 个，其中启用 ${item.usage.enabled} 个`}>
                        {item.usage.total === 0 ? '未使用' : `账号 ${item.usage.enabled}/${item.usage.total}`}
                      </span>
                      {item.liveness ? (
                        <Badge tone={latencyTone(item.liveness)} title={item.liveness.message}>{item.liveness.success ? `${item.liveness.durationMs}ms` : '不可用'}</Badge>
                      ) : (
                        <Badge tone="neutral">未测</Badge>
                      )}
                      {position !== undefined && <Badge tone="primary" title="轮换顺序">第 {position}</Badge>}
                    </label>
                  </li>
                )
              })}
            </ol>
          ) : (
            <div className="rounded-md border border-dashed px-3 py-2 text-xs text-muted-foreground">暂无可用代理资源</div>
          )}
          <p className="text-[0.7rem] leading-4 text-muted-foreground">
            已选 {value.resourceIds.length} 个，按"第 N"的顺序循环分配给账号 1、2、3…；可用 {liveCount}/{proxyResources.length}。
          </p>
        </div>
      )}

      {value.mode === 'direct_url' && (
        <FieldGrid>
          <Field label="代理 URL"><Input value={value.url} onChange={(event) => onChange({ ...value, url: event.target.value })} placeholder="socks5h://host:port" disabled={disabled} /></Field>
          <Field label="用户名"><SecretInput value={value.username} onChange={(next) => onChange({ ...value, username: next })} visible={showSecret} onToggle={() => setShowSecret((v) => !v)} placeholder="可选" disabled={disabled} /></Field>
          <Field label="密码"><SecretInput value={value.password} onChange={(next) => onChange({ ...value, password: next })} visible={showSecret} onToggle={() => setShowSecret((v) => !v)} placeholder="可选" disabled={disabled} /></Field>
        </FieldGrid>
      )}
    </div>
  )
}
