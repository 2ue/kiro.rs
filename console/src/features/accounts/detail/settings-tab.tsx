import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { credentialsApi } from '@/api/endpoints/credentials'
import { errorMessage } from '@/api/client'
import { NumberInput, SelectControl, SettingRow, ToggleControl } from '@/components/patterns/fields'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { ChevronDown } from 'lucide-react'
import { qk } from '@/queries/keys'
import { useProxies } from '@/queries/shared'
import type { AccountRow } from '../queries'

interface Draft {
  priority: number
  concurrency: number | null
  rpm: number | null
  rateLimitAutoDisable: boolean
  proxyMode: 'inherit' | 'direct' | 'resource' | 'custom'
  proxyResourceId: number | null
  proxyUrl: string
  proxyUsername: string
  proxyPassword: string
  region: string
  authRegion: string
  apiRegion: string
  warmup: number
}

function fromRow(r: AccountRow): Draft {
  return {
    priority: r.priority,
    concurrency: r.maxConcurrentRequestsOverride ?? null,
    rpm: r.rpmOverride ?? null,
    rateLimitAutoDisable: r.rateLimitAutoDisableEnabled,
    proxyMode: r.proxyResourceId ? 'resource' : r.proxyUrl === 'direct' ? 'direct' : r.proxyUrl ? 'custom' : 'inherit',
    proxyResourceId: r.proxyResourceId ?? null,
    proxyUrl: r.proxyUrl && r.proxyUrl !== 'direct' ? r.proxyUrl : '',
    proxyUsername: r.proxyUsername ?? '',
    proxyPassword: r.proxyPassword ?? '',
    region: r.region ?? '',
    authRegion: r.authRegion ?? '',
    apiRegion: r.apiRegion ?? '',
    warmup: r.warmupRemaining,
  }
}

const proxyKey = (d: Draft) => JSON.stringify([d.proxyMode, d.proxyResourceId, d.proxyUrl, d.proxyUsername, d.proxyPassword])
const regionKey = (d: Draft) => JSON.stringify([d.region, d.authRegion, d.apiRegion])

/** 账号设置：草稿编辑，统一保存；只提交修改过的分组 */
export function SettingsTab({ row }: { row: AccountRow }) {
  const queryClient = useQueryClient()
  const base = useMemo(() => fromRow(row), [row])
  const [draft, setDraft] = useState(base)
  const [touched, setTouched] = useState(false)
  const proxies = useProxies()

  // 服务端数据更新时，只有在用户未编辑时才同步
  const [syncedBase, setSyncedBase] = useState(base)
  if (syncedBase !== base) {
    setSyncedBase(base)
    if (!touched) setDraft(base)
  }

  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => {
    setTouched(true)
    setDraft((d) => ({ ...d, [key]: value }))
  }

  const dirty = {
    priority: draft.priority !== base.priority,
    concurrency: draft.concurrency !== base.concurrency,
    rpm: draft.rpm !== base.rpm,
    rateLimitAutoDisable: draft.rateLimitAutoDisable !== base.rateLimitAutoDisable,
    proxy: proxyKey(draft) !== proxyKey(base),
    region: regionKey(draft) !== regionKey(base),
    warmup: draft.warmup !== base.warmup,
  }
  const dirtyCount = Object.values(dirty).filter(Boolean).length

  const save = useMutation({
    mutationFn: async () => {
      const id = row.id
      const tasks: Array<[string, () => Promise<unknown>]> = []
      if (dirty.priority) tasks.push(['优先级', () => credentialsApi.setPriority(id, draft.priority)])
      if (dirty.concurrency) tasks.push(['并发', () => credentialsApi.setConcurrency(id, draft.concurrency)])
      if (dirty.rpm) tasks.push(['RPM', () => credentialsApi.setRpm(id, draft.rpm)])
      if (dirty.rateLimitAutoDisable)
        tasks.push(['限流自动禁用', () => credentialsApi.setRateLimitAutoDisable(id, draft.rateLimitAutoDisable)])
      if (dirty.warmup) tasks.push(['预热', () => credentialsApi.setWarmup(id, draft.warmup)])
      if (dirty.region)
        tasks.push([
          'Region',
          () =>
            credentialsApi.setRegions(id, {
              region: draft.region.trim() || null,
              authRegion: draft.authRegion.trim() || null,
              apiRegion: draft.apiRegion.trim() || null,
            }),
        ])
      if (dirty.proxy)
        tasks.push([
          '代理',
          () =>
            credentialsApi.setProxy(
              id,
              draft.proxyMode === 'resource'
                ? { proxyResourceId: draft.proxyResourceId }
                : draft.proxyMode === 'custom'
                  ? {
                      proxyResourceId: null,
                      proxyUrl: draft.proxyUrl.trim(),
                      proxyUsername: draft.proxyUsername.trim() || undefined,
                      proxyPassword: draft.proxyPassword || undefined,
                    }
                  : draft.proxyMode === 'direct'
                    ? { proxyResourceId: null, proxyUrl: 'direct' }
                    : { proxyResourceId: null, proxyUrl: '' },
            ),
        ])
      const failures: string[] = []
      for (const [name, run] of tasks) {
        try {
          await run()
        } catch (e) {
          failures.push(`${name}：${errorMessage(e)}`)
        }
      }
      return { total: tasks.length, failures }
    },
    onSuccess: ({ total, failures }) => {
      if (failures.length) toast.error(`${failures.length}/${total} 项保存失败`, { description: failures.join('\n') })
      else toast.success('账号设置已保存')
      setTouched(false)
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: qk.credentials.all }),
    meta: { error: '保存失败' },
  })

  const proxyOptions = proxies.data?.resources ?? []

  return (
    <div className="flex h-full flex-col">
      <div className="divide-y">
        <SettingRow label="优先级" description="数字越小越优先；仅在优先级模式下起决定作用" dirty={dirty.priority} onReset={() => set('priority', base.priority)}>
          <NumberInput value={draft.priority} min={0} onChange={(v) => set('priority', v ?? 0)} />
        </SettingRow>
        <SettingRow label="最大并发" description="留空表示继承全局设置；0 表示不限制" dirty={dirty.concurrency} onReset={() => set('concurrency', base.concurrency)}>
          <NumberInput value={draft.concurrency} min={0} allowEmpty placeholder="继承全局" suffix="并发" onChange={(v) => set('concurrency', v)} />
        </SettingRow>
        <SettingRow label="每分钟请求上限" description="留空表示继承全局设置；0 表示不限速" dirty={dirty.rpm} onReset={() => set('rpm', base.rpm)}>
          <NumberInput value={draft.rpm} min={0} allowEmpty placeholder="继承全局" suffix="RPM" onChange={(v) => set('rpm', v)} />
        </SettingRow>
        <SettingRow label="限流后自动禁用" description="收到上游限流时直接禁用该账号，而不是冷却后重试" dirty={dirty.rateLimitAutoDisable} onReset={() => set('rateLimitAutoDisable', base.rateLimitAutoDisable)}>
          <ToggleControl checked={draft.rateLimitAutoDisable} onChange={(v) => set('rateLimitAutoDisable', v)} />
        </SettingRow>
        <SettingRow label="出站代理" description="账号代理优先于全局代理" dirty={dirty.proxy} onReset={() => setDraft((d) => ({ ...d, ...pickProxy(base) }))} stacked>
          <div className="space-y-2">
            <SelectControl
              value={draft.proxyMode}
              onChange={(v) => set('proxyMode', v)}
              options={[
                { value: 'inherit', label: '继承全局代理' },
                { value: 'direct', label: '强制直连（忽略全局代理）' },
                { value: 'resource', label: '使用代理资源' },
                { value: 'custom', label: '自定义代理地址' },
              ]}
            />
            {draft.proxyMode === 'resource' && (
              <SelectControl
                value={draft.proxyResourceId ? String(draft.proxyResourceId) : ''}
                onChange={(v) => set('proxyResourceId', Number(v))}
                options={proxyOptions.map((p) => ({ value: String(p.id), label: `${p.name}${p.enabled ? '' : '（已禁用）'}` }))}
              />
            )}
            {draft.proxyMode === 'custom' && (
              <div className="grid gap-2 sm:grid-cols-3">
                <Input className="sm:col-span-3" placeholder="http://host:port 或 socks5://host:port" value={draft.proxyUrl} onChange={(e) => set('proxyUrl', e.target.value)} />
                <Input placeholder="用户名（可选）" value={draft.proxyUsername} onChange={(e) => set('proxyUsername', e.target.value)} />
                <Input type="password" placeholder="密码（可选）" value={draft.proxyPassword} onChange={(e) => set('proxyPassword', e.target.value)} className="sm:col-span-2" />
              </div>
            )}
          </div>
        </SettingRow>
      </div>

      <Collapsible className="mt-2">
        <CollapsibleTrigger className="group flex w-full items-center gap-1 py-2 text-xs font-medium text-muted-foreground hover:text-foreground">
          <ChevronDown className="size-3.5 transition-transform group-data-[state=open]:rotate-180" />
          高级设置
        </CollapsibleTrigger>
        <CollapsibleContent className="divide-y">
          <SettingRow label="Region" description="同时设置认证与 API Region；留空使用默认" dirty={dirty.region} onReset={() => setDraft((d) => ({ ...d, region: base.region, authRegion: base.authRegion, apiRegion: base.apiRegion }))} stacked>
            <div className="grid gap-2 sm:grid-cols-3">
              <Input placeholder="region" value={draft.region} onChange={(e) => set('region', e.target.value)} />
              <Input placeholder="authRegion" value={draft.authRegion} onChange={(e) => set('authRegion', e.target.value)} />
              <Input placeholder="apiRegion" value={draft.apiRegion} onChange={(e) => set('apiRegion', e.target.value)} />
            </div>
          </SettingRow>
          <SettingRow label="剩余预热请求" description="大于 0 时按预热比例参与调度" dirty={dirty.warmup} onReset={() => set('warmup', base.warmup)}>
            <NumberInput value={draft.warmup} min={0} suffix="次" onChange={(v) => set('warmup', v ?? 0)} />
          </SettingRow>
        </CollapsibleContent>
      </Collapsible>

      {dirtyCount > 0 && (
        <div className="sticky bottom-0 mt-4 flex items-center justify-between gap-2 border-t bg-popover py-3">
          <span className="text-xs text-muted-foreground">{dirtyCount} 项修改未保存</span>
          <div className="flex gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setDraft(base)
                setTouched(false)
              }}
            >
              放弃
            </Button>
            <Button size="sm" onClick={() => save.mutate()} disabled={save.isPending || (draft.proxyMode === 'resource' && !draft.proxyResourceId) || (draft.proxyMode === 'custom' && !draft.proxyUrl.trim())}>
              保存
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}

function pickProxy(d: Draft): Partial<Draft> {
  return { proxyMode: d.proxyMode, proxyResourceId: d.proxyResourceId, proxyUrl: d.proxyUrl, proxyUsername: d.proxyUsername, proxyPassword: d.proxyPassword }
}
