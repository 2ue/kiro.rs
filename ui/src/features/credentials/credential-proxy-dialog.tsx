import { ArrowRight, Search } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Badge, Button, Input, Spinner } from '@/components/ui'
import { EmptyState, Field, FieldGrid, LoadingState, ModalShell } from '@/components/patterns'
import { useProxyResources, useRuntimeConfig, useSetCredentialProxy } from '@/hooks/use-credentials'
import { formatDateTime } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import type { CredentialStatusItem, ProxyResource } from '@/types/api'
import { SecretInput } from './credential-inputs'
import { displayProxyUrl, proxyChoice, type ProxyChoice } from './credential-utils'

/** Searching only helps once the list no longer fits on one screen. */
const SEARCH_MIN_RESOURCES = 6

interface ProxyDraft {
  resourceId: string
  url: string
  username: string
  password: string
}

function savedDraft(credential: CredentialStatusItem): ProxyDraft {
  return {
    resourceId: credential.proxyResourceId ? String(credential.proxyResourceId) : '',
    url: credential.proxyUrl || '',
    username: credential.proxyUsername || '',
    password: credential.proxyPassword || '',
  }
}

function sameDraft(a: ProxyDraft, b: ProxyDraft): boolean {
  if (a.resourceId || b.resourceId) return a.resourceId === b.resourceId
  return a.url.trim() === b.url.trim() && a.username.trim() === b.username.trim() && a.password.trim() === b.password.trim()
}

function testSummary(r: ProxyResource): { text: string; ok: boolean } | undefined {
  if (!r.lastTestAt || r.lastTestSuccess == null) return undefined
  const duration = typeof r.lastTestDurationMs === 'number' ? ` ${r.lastTestDurationMs}ms` : ''
  return { text: `${r.lastTestSuccess ? '测试成功' : '测试失败'}${duration} · ${formatDateTime(r.lastTestAt)}`, ok: r.lastTestSuccess }
}

/** One proxy choice as the pinned summary shows it. */
function ChoiceLine({ choice, resource, lead }: { choice: ProxyChoice; resource?: ProxyResource; lead: string }) {
  const test = resource ? testSummary(resource) : undefined
  return (
    <div className="flex min-w-0 items-start gap-3">
      <span className="w-12 shrink-0 pt-0.5 text-xs text-muted-foreground">{lead}</span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="font-semibold text-foreground">{choice.title}</span>
          {choice.resourceId !== undefined && <span className="font-mono text-xs text-muted-foreground">#{choice.resourceId}</span>}
          {resource && <Badge tone={resource.enabled ? 'success' : 'error'}>{resource.enabled ? '启用' : '禁用'}</Badge>}
          {test && <span className={cn('text-xs', test.ok ? 'text-muted-foreground' : 'text-destructive')}>{test.text}</span>}
        </div>
        {choice.detail && <div className="mt-0.5 break-all font-mono text-xs text-muted-foreground">{choice.detail}</div>}
        {choice.blocked && <div className="mt-1 text-xs font-medium text-destructive">{choice.blocked}</div>}
      </div>
    </div>
  )
}

/**
 * Binds a credential to a proxy. What the account uses now, and what it will switch to, stay pinned
 * above the list, so the choice is visible without scrolling; the list below is for picking another.
 */
export function CredentialProxyDialog({ open, onClose, credential, title }: {
  open: boolean
  onClose: () => void
  credential: CredentialStatusItem
  title: string
}) {
  const proxyResources = useProxyResources()
  const setCredentialProxy = useSetCredentialProxy()
  const resources = proxyResources.data?.resources
  const [draft, setDraft] = useState<ProxyDraft>(() => savedDraft(credential))
  const [showUsername, setShowUsername] = useState(false)
  const [showPassword, setShowPassword] = useState(false)
  const [query, setQuery] = useState('')

  const saved = savedDraft(credential)
  // Start from what is saved each time the dialog opens, or when the saved binding changes underneath it.
  useEffect(() => {
    if (!open) return
    setDraft(savedDraft(credential))
    setShowUsername(false)
    setShowPassword(false)
    setQuery('')
  }, [open, credential.id, credential.proxyResourceId, credential.proxyUrl, credential.proxyUsername, credential.proxyPassword])

  const runtimeConfig = useRuntimeConfig()
  // The account's snapshot already resolved the global proxy when it is the one in use.
  const globalProxyUrl = runtimeConfig.data?.proxyUrl
    ?? (credential.effectiveProxySource === 'global' ? credential.effectiveProxyUrl : undefined)
  const current = proxyChoice(
    { resourceId: credential.proxyResourceId, url: credential.proxyUrl },
    resources,
    { resourceName: credential.proxyResourceName, globalProxyUrl },
  )
  const changed = !sameDraft(draft, saved)
  const next = proxyChoice(
    { resourceId: draft.resourceId ? Number(draft.resourceId) : null, url: draft.url },
    resources,
    { globalProxyUrl },
  )
  const resourceById = (id?: number) => (id === undefined ? undefined : resources?.find((r) => r.id === id))

  const visibleResources = useMemo(() => {
    const list = resources ?? []
    const q = query.trim().toLowerCase().replace(/^#/, '')
    if (!q) return list
    return list.filter((r) => String(r.id) === q || r.name.toLowerCase().includes(q) || r.proxyUrl.toLowerCase().includes(q))
  }, [resources, query])

  const pending = setCredentialProxy.isPending
  const close = () => { if (!pending) onClose() }
  const pickResource = (id: number) => setDraft({ resourceId: String(id), url: '', username: '', password: '' })
  // Typing a direct proxy leaves any picked resource, the same as picking "不绑定".
  const editDirect = (patch: Partial<ProxyDraft>) => setDraft((d) => ({ ...d, ...patch, resourceId: '' }))

  const save = () => {
    const url = draft.url.trim()
    if (!draft.resourceId && !url && (draft.username.trim() || draft.password.trim())) {
      toast.error('直接代理 URL 为空时不能单独设置用户名/密码')
      return
    }
    setCredentialProxy.mutate({
      id: credential.id,
      request: {
        proxyResourceId: draft.resourceId ? Number(draft.resourceId) : null,
        proxyUrl: draft.resourceId ? undefined : url || undefined,
        proxyUsername: draft.resourceId ? undefined : draft.username.trim() || undefined,
        proxyPassword: draft.resourceId ? undefined : draft.password.trim() || undefined,
      },
    }, {
      onSuccess: (res) => { toast.success(res.message); onClose() },
      onError: (e) => toast.error(`代理设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const unbound = !draft.resourceId
  return (
    <ModalShell open={open} title={title} width="max-w-xl" onClose={close}
      pinned={(
        <div className="space-y-2.5 text-sm" aria-live="polite">
          <ChoiceLine lead="当前" choice={current} resource={resourceById(current.resourceId)} />
          {changed && (
            <div className="flex items-start gap-2 rounded-lg border border-primary/40 bg-primary/5 p-2.5">
              <ArrowRight className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden="true" />
              <div className="min-w-0 flex-1">
                <ChoiceLine lead="改为" choice={next} resource={resourceById(next.resourceId)} />
              </div>
              <Button variant="ghost" size="xs" onClick={() => setDraft(saved)} disabled={pending}>撤销</Button>
            </div>
          )}
        </div>
      )}
      footer={(
        <>
          <Button variant="ghost" size="sm" onClick={close} disabled={pending}>取消</Button>
          <Button size="sm" onClick={save} disabled={pending || !changed}>
            {pending && <Spinner size="sm" />}保存
          </Button>
        </>
      )}>
      <div className="space-y-2">
        <div className="flex items-center justify-between gap-3">
          <h3 className="text-sm font-semibold">更换为</h3>
          {(resources?.length ?? 0) >= SEARCH_MIN_RESOURCES && (
            <div className="relative w-56 max-w-[60%]">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden="true" />
              <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="名称 / 地址 / #ID"
                aria-label="搜索代理资源" className="h-8 pl-8 text-xs" />
            </div>
          )}
        </div>

        <div role="radiogroup" aria-label="选择代理" className="space-y-1.5">
          <div className={cn('rounded-lg border transition-colors', unbound ? 'border-primary bg-primary/5' : 'border-border')}>
            <button type="button" role="radio" aria-checked={unbound}
              className="w-full rounded-lg p-2.5 text-left text-sm hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              onClick={() => setDraft((d) => (d.resourceId ? { ...d, resourceId: '' } : d))}>
              <div className="flex items-center gap-1.5">
                <span className="font-semibold">不绑定代理资源</span>
                {!saved.resourceId && <Badge tone="neutral">当前</Badge>}
              </div>
              <div className="mt-0.5 text-xs text-muted-foreground">填写下面的账号直连代理；留空则使用全局代理，没有全局代理时直接连接上游。</div>
            </button>
            {unbound && (
              <div className="border-t border-border/70 p-2.5">
                <FieldGrid>
                  <Field label="代理 URL">
                    <Input value={draft.url} placeholder="socks5h://127.0.0.1:1080" disabled={pending}
                      onChange={(e) => editDirect({ url: e.target.value })} />
                  </Field>
                  <Field label="用户名">
                    <SecretInput value={draft.username} onChange={(v) => editDirect({ username: v })}
                      visible={showUsername} onToggle={() => setShowUsername((v) => !v)} disabled={pending} placeholder="可选" />
                  </Field>
                  <Field label="密码">
                    <SecretInput value={draft.password} onChange={(v) => editDirect({ password: v })}
                      visible={showPassword} onToggle={() => setShowPassword((v) => !v)} disabled={pending} placeholder="可选" />
                  </Field>
                </FieldGrid>
              </div>
            )}
          </div>

          {proxyResources.isLoading ? (
            <LoadingState text="加载代理资源..." />
          ) : (resources?.length ?? 0) === 0 ? (
            <EmptyState title="暂无代理资源" description="请先在代理页添加" />
          ) : visibleResources.length === 0 ? (
            <EmptyState title="没有匹配的代理资源" description="换个名称、地址或 #ID 试试" />
          ) : (
            visibleResources.map((r) => {
              const selected = draft.resourceId === String(r.id)
              const isCurrent = saved.resourceId === String(r.id)
              const test = testSummary(r)
              return (
                <button key={r.id} type="button" role="radio" aria-checked={selected}
                  className={cn(
                    'w-full rounded-lg border p-2.5 text-left text-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                    selected ? 'border-primary bg-primary/5' : r.enabled ? 'border-border hover:bg-muted' : 'border-destructive/25 bg-destructive/5 opacity-70 hover:opacity-100',
                  )}
                  onClick={() => pickResource(r.id)}>
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="font-semibold">{r.name}</span>
                    <span className="font-mono text-xs text-muted-foreground">#{r.id}</span>
                    <Badge tone={r.enabled ? 'success' : 'error'}>{r.enabled ? '启用' : '禁用'}</Badge>
                    {isCurrent && <Badge tone="neutral">当前</Badge>}
                    {selected && !isCurrent && <Badge tone="primary">已选</Badge>}
                  </div>
                  <div className="mt-0.5 truncate font-mono text-xs text-muted-foreground">{displayProxyUrl(r.proxyUrl)}</div>
                  <div className="mt-0.5 text-xs text-muted-foreground">
                    已绑定 {r.credentialCount} 个账号
                    {test && <span className={test.ok ? undefined : 'text-destructive'}> · {test.text}</span>}
                  </div>
                </button>
              )
            })
          )}
        </div>
      </div>
    </ModalShell>
  )
}
