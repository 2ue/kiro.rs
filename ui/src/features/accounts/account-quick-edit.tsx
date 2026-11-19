// 卡片上的单字段快捷编辑：优先级、并发、成本倍率、路由限制、支持模型、代理。保存走 PUT 部分字段。
import { useState, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { discoverStoredAccountSupportedModels, updateAccount } from '@/api/accounts'
import { getRouteScenarioOptions } from '@/api/route-scenarios'
import { Field, ModalShell, useConfirm } from '@/components/patterns'
import { Button, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui'
import { RouteSelector } from '@/features/route-scenarios/route-selector'
import { SupportedModelTagsEditor } from '@/features/shared/supported-model-tags'
import { RULE_MODE_OPTIONS } from '@/lib/enum-labels'
import { extractErrorMessage } from '@/lib/utils'
import type { Account, AccountStatus, RouteRuleMode, UpdateAccountRequest } from '@/types/api'
import { NumberInput } from './account-form-parts'
import { PROXY_MASK_HINT, hasMaskedProxyCredentials } from './account-utils'

export type QuickEditField = 'priority' | 'maxConcurrentRequests' | 'costMultiplier' | 'route' | 'supportedModels' | 'proxy'

const TITLES: Record<QuickEditField, string> = {
  priority: '优先级',
  maxConcurrentRequests: '单账号最大并发',
  costMultiplier: '成本倍率',
  route: '路由限制',
  supportedModels: '支持模型',
  proxy: '代理',
}

function InfoBox({ rows, children }: { rows: Array<[string, string]>; children?: ReactNode }) {
  return (
    <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
      {rows.map(([label, value]) => (
        <div key={label} className="flex justify-between gap-2">
          <span className="text-muted-foreground">{label}</span>
          <span className="font-semibold tabular-nums">{value}</span>
        </div>
      ))}
      {children && <div className="mt-1 text-xs text-muted-foreground">{children}</div>}
    </div>
  )
}

/** 打开时由页面按账号和字段加 key 重新挂载，所以初值直接从 props 取。 */
export function AccountQuickEditModal({ target, status, onClose, onSaved }: {
  target: { account: Account; field: QuickEditField } | null
  status?: AccountStatus
  onClose: () => void
  onSaved: () => void
}) {
  const confirm = useConfirm()
  const account = target?.account
  const field = target?.field
  const [text, setText] = useState(() =>
    !account ? ''
      : field === 'priority' ? String(account.priority)
        : field === 'maxConcurrentRequests' ? String(account.maxConcurrentRequests)
          : field === 'costMultiplier' ? String(account.costMultiplier)
            // 代理预填当前（打码的）地址：只改主机或端口时保留 ***，后端会沿用已保存的用户名和密码。
            : field === 'proxy' ? account.proxyUrl ?? ''
              : '',
  )
  const [routeMode, setRouteMode] = useState<RouteRuleMode>(account?.routeMode || 'allow_all')
  const [routeRules, setRouteRules] = useState<string[]>(() => [...(account?.routeRules ?? [])])
  const [models, setModels] = useState<string[]>(() => [...(account?.supportedModels ?? [])])
  const [saving, setSaving] = useState(false)
  const [discovering, setDiscovering] = useState(false)
  const routes = useQuery({ queryKey: ['route-scenario-options'], queryFn: getRouteScenarioOptions, enabled: field === 'route' })

  if (!account || !field) return null
  const label = `#${account.id} ${account.name}`

  const save = async (request: UpdateAccountRequest, success: string) => {
    setSaving(true)
    try {
      await updateAccount(account.id, request)
      toast.success(success)
      onSaved()
      onClose()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  const submit = () => {
    if (field === 'priority') {
      const value = Number(text)
      if (!text.trim() || !Number.isInteger(value)) return toast.error('优先级必须是整数')
      return save({ priority: value }, `优先级已改为 ${value}`)
    }
    if (field === 'maxConcurrentRequests') {
      const value = Number(text)
      if (!Number.isInteger(value) || value < 1 || value > 10000) return toast.error('单账号最大并发必须是 1 到 10000 的整数')
      return save({ maxConcurrentRequests: value }, `并发上限已改为 ${value}`)
    }
    if (field === 'costMultiplier') {
      const value = Number(text)
      if (!text.trim() || !Number.isFinite(value) || value <= 0) return toast.error('成本倍率必须大于 0')
      return save({ costMultiplier: value }, `成本倍率已改为 ×${value}`)
    }
    if (field === 'route') {
      const rules = routeMode === 'allow_all' ? [] : routeRules
      if (routeMode === 'allow_list' && rules.length === 0) return toast.error('只允许模式至少需要一条路由，否则这个账号不会承接任何请求')
      return save({ routeMode, routeRules: rules }, '路由限制已保存')
    }
    if (field === 'supportedModels') return save({ supportedModels: models }, models.length ? `支持模型已保存（${models.length} 个）` : '已改为不限制模型')
    const url = text.trim()
    if (!url) return toast.error('请填写代理地址；要去掉代理请点“清除代理”')
    if (url === (account.proxyUrl ?? '').trim()) {
      toast.info('没有改动')
      return onClose()
    }
    if (!/^(https?|socks5h?):\/\/[^\s]+$/i.test(url)) return toast.error('代理地址必须以 http://、https://、socks5:// 或 socks5h:// 开头')
    return save({ proxyUrl: url }, '代理已保存')
  }

  const discover = async () => {
    setDiscovering(true)
    try {
      const found = (await discoverStoredAccountSupportedModels(account.id)).supportedModels
      const removed = models.filter((model) => !found.includes(model))
      if (removed.length > 0) {
        const ok = await confirm({
          title: '用上游结果覆盖当前列表？',
          message: `上游返回 ${found.length} 个模型，会移除当前列表里的 ${removed.length} 个：${removed.join('、')}。保存后该账号不再承接这些模型。`,
          confirmText: '覆盖',
          tone: 'danger',
        })
        if (!ok) return
      }
      setModels(found)
      toast.success(`上游返回 ${found.length} 个模型，确认后点保存`)
    } catch (error) {
      toast.error(extractErrorMessage(error))
    } finally {
      setDiscovering(false)
    }
  }

  const clearProxy = async () => {
    const ok = await confirm({
      title: '清除这个账号的代理？',
      message: `保存后「${account.name}」不再走自己的代理，改走全局代理或直连。`,
      confirmText: '清除代理',
      tone: 'danger',
    })
    if (ok) await save({ proxyUrl: null }, '代理已清除')
  }

  const busy = saving || discovering
  const proxyMasked = hasMaskedProxyCredentials(account.proxyUrl)

  return (
    <ModalShell
      open
      title={`${TITLES[field]}：${label}`}
      width={field === 'route' || field === 'supportedModels' ? 'max-w-xl' : 'max-w-md'}
      onClose={() => { if (!busy) onClose() }}
      footer={(
        <>
          {field === 'supportedModels' && (
            <>
              <Button variant="ghost" size="sm" className="mr-auto" disabled={busy || (account.supportedModels ?? []).length === 0} onClick={() => save({ supportedModels: [] }, '已改为不限制模型')}>不限制</Button>
              <Button variant="outline" size="sm" disabled={busy} onClick={() => void discover()}>
                {discovering && <Loader2 className="animate-spin" />}从上游发现
              </Button>
            </>
          )}
          {field === 'proxy' && account.proxyUrl && (
            <Button variant="ghost" size="sm" className="mr-auto text-destructive hover:bg-destructive/10 hover:text-destructive" disabled={busy} onClick={() => void clearProxy()}>清除代理</Button>
          )}
          <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>取消</Button>
          <Button size="sm" onClick={() => void submit()} disabled={busy}>
            {saving && <Loader2 className="animate-spin" />}保存
          </Button>
        </>
      )}
    >
      <div className="space-y-3">
        {field === 'priority' && (
          <>
            <InfoBox rows={[['当前优先级', String(account.priority)]]}>数字越小越靠前；同优先级再按容量和状态分配。</InfoBox>
            <Field label="新优先级" htmlFor="quick-priority">
              <NumberInput id="quick-priority" value={text} onChange={setText} />
            </Field>
          </>
        )}
        {field === 'maxConcurrentRequests' && (
          <>
            <InfoBox rows={[['当前在飞 / 上限', `${status?.inFlight ?? 0} / ${account.maxConcurrentRequests}`]]}>
              当前账号同时处理的最大请求数，不是 RPM；调小后已在飞的请求照常完成。
            </InfoBox>
            <Field label="单账号最大并发" htmlFor="quick-concurrency" description="1 到 10000">
              <NumberInput id="quick-concurrency" value={text} onChange={setText} min={1} max={10000} unit="个请求" />
            </Field>
          </>
        )}
        {field === 'costMultiplier' && (
          <>
            <InfoBox rows={[['当前倍率', `×${account.costMultiplier}`]]}>账号成本 = 上游原始成本 × 倍率，用于统计里的账号成本。</InfoBox>
            <Field label="成本倍率" htmlFor="quick-cost">
              <NumberInput id="quick-cost" value={text} onChange={setText} step="0.01" min={0} unit="×" />
            </Field>
          </>
        )}
        {field === 'route' && (
          <>
            <div className="rounded-lg bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
              控制哪些入口路由可以调度到这个账号，被排除的路由不会用这个账号。要整套切换多个账号的规则，用路由场景。
            </div>
            <div className="grid gap-3 sm:grid-cols-[12rem_1fr]">
              <Field label="路由模式">
                <Select value={routeMode} onValueChange={(v) => setRouteMode(v as RouteRuleMode)} disabled={busy}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {RULE_MODE_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
                  </SelectContent>
                </Select>
              </Field>
              <Field label="路由">
                <RouteSelector routes={routes.data?.routes ?? []} value={routeRules} disabled={busy || routeMode === 'allow_all'} onChange={setRouteRules} />
              </Field>
            </div>
          </>
        )}
        {field === 'supportedModels' && (
          <>
            <InfoBox rows={[['当前限制', (account.supportedModels ?? []).length ? `${account.supportedModels.length} 个模型` : '不限制']]}>
              只决定该账号是否允许承接某些模型；空列表表示不限制。不改变请求体里的 model，也不影响模型映射规则。
            </InfoBox>
            <SupportedModelTagsEditor value={models} disabled={busy} onChange={setModels} />
          </>
        )}
        {field === 'proxy' && (
          <>
            <InfoBox rows={[['当前代理', account.proxyUrl || '未配置（走全局代理或直连）']]}>
              {proxyMasked ? PROXY_MASK_HINT : '填写这个账号自己的代理；不填时走全局代理或直连。'}
            </InfoBox>
            <Field label="代理地址" htmlFor="quick-proxy" description="支持 http、https、socks5、socks5h">
              <Input id="quick-proxy" value={text} autoComplete="off" placeholder="http://user:pass@host:port" disabled={busy} onChange={(e) => setText(e.target.value)} />
            </Field>
          </>
        )}
      </div>
    </ModalShell>
  )
}
