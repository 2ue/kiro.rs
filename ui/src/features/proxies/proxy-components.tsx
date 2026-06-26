import * as React from 'react'
import { Activity, ChevronDown, ChevronUp, Edit3, Eye, EyeOff, Plus, Trash2, Users } from 'lucide-react'
import { toast } from 'sonner'
import { formatDate } from '@/lib/format'
import { extractErrorMessage, cn } from '@/lib/utils'
import {
  useCreateProxyResource,
  useCredentials,
  useDeleteProxyResource,
  useSetCredentialProxy,
  useTestProxyResource,
  useUpdateProxyResource,
} from '@/hooks/use-credentials'
import type { CredentialStatusItem, ProxyResource, ProxyResourceTestResponse } from '@/types/api'
import { ModalShell, Field, FieldGrid, EmptyState, LoadingState, useConfirm } from '@/components/patterns'
import { Badge, Button, Checkbox, Input, Textarea, Switch, Spinner } from '@/components/ui'

// ============================================================================
// ProxyTestResult
// ============================================================================

export function ProxyTestResult({ result }: { result: ProxyResourceTestResponse | null }) {
  if (!result) return null
  return (
    <div className={cn(
      'rounded-lg border p-3 text-xs',
      result.success
        ? 'border-success/30 bg-success/5 text-success'
        : 'border-destructive/30 bg-destructive/5 text-destructive'
    )}>
      <div className="font-semibold">{result.message}</div>
      <div className="mt-1 text-muted-foreground">
        耗时 {result.durationMs}ms
        {typeof result.status === 'number' ? ` · HTTP ${result.status}` : ''} · {result.testUrl}
      </div>
      {result.responsePreview && (
        <div className="mt-1 break-words font-mono text-foreground/70">{result.responsePreview}</div>
      )}
    </div>
  )
}

// ============================================================================
// SecretInput
// ============================================================================

function SecretInput({ value, onChange, visible, onToggle }: {
  value: string; onChange: (v: string) => void; visible: boolean; onToggle: () => void
}) {
  return (
    <div className="relative">
      <Input
        className="pr-10"
        type={visible ? 'text' : 'password'}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
      <Button
        variant="ghost"
        size="icon-xs"
        className="absolute right-1 top-1/2 -translate-y-1/2"
        onClick={onToggle}
        title={visible ? '隐藏' : '显示'}
      >
        {visible ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
      </Button>
    </div>
  )
}

// ============================================================================
// CredentialBindingPicker
// ============================================================================

const credentialLabel = (c: CredentialStatusItem) => c.email || c.maskedApiKey || `账号 #${c.id}`

export function CredentialBindingPicker({ credentials, selectedIds, onToggle }: {
  credentials: CredentialStatusItem[]; selectedIds: Set<number>; onToggle: (id: number) => void
}) {
  if (!credentials.length) return <EmptyState title="暂无可绑定账号" />
  const sorted = [...credentials].sort((a, b) => {
    if (a.disabled !== b.disabled) return a.disabled ? 1 : -1
    return a.id - b.id
  })
  return (
    <div className="scrollbar-thin max-h-72 space-y-2 overflow-y-auto rounded-xl border border-border bg-muted/30 p-2">
      {sorted.map((credential) => {
        const selected = selectedIds.has(credential.id)
        return (
          <label
            key={credential.id}
            className={cn(
              'flex cursor-pointer items-start gap-3 rounded-lg border p-2 text-sm transition-colors',
              selected
                ? 'border-primary bg-primary/5'
                : credential.disabled
                  ? 'border-destructive/25 bg-destructive/5 opacity-80'
                  : 'border-border bg-card hover:bg-muted'
            )}
          >
            <Checkbox className="mt-0.5" checked={selected} onCheckedChange={() => onToggle(credential.id)} />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="truncate font-semibold" title={credentialLabel(credential)}>
                  {credentialLabel(credential)}
                </span>
                <Badge>#{credential.id}</Badge>
                <Badge tone={credential.disabled ? 'error' : 'success'}>
                  {credential.disabled ? '已禁用' : '启用'}
                </Badge>
                {credential.proxyResourceName && <Badge tone="info">{credential.proxyResourceName}</Badge>}
              </div>
              <div className="mt-1 truncate text-xs text-muted-foreground">
                {credential.effectiveProxyUrl || '当前未使用代理资源'}
              </div>
            </div>
          </label>
        )
      })}
    </div>
  )
}

// ============================================================================
// ProxyEditorModal
// ============================================================================

type ProxyForm = {
  name: string; proxyUrl: string; proxyUsername: string; proxyPassword: string; notes: string; enabled: boolean
}

const emptyForm = (): ProxyForm => ({ name: '', proxyUrl: '', proxyUsername: '', proxyPassword: '', notes: '', enabled: true })

const formFromResource = (r: ProxyResource): ProxyForm => ({
  name: r.name, proxyUrl: r.proxyUrl, proxyUsername: r.proxyUsername || '',
  proxyPassword: r.proxyPassword || '', notes: r.notes || '', enabled: r.enabled,
})

export function ProxyEditorModal({ open, resource, onClose }: {
  open: boolean; resource: ProxyResource | null; onClose: () => void
}) {
  const [form, setForm] = React.useState(() => (resource ? formFromResource(resource) : emptyForm()))
  const [selectedIds, setSelectedIds] = React.useState<Set<number>>(new Set())
  const [bindingReady, setBindingReady] = React.useState(false)
  const [showUsername, setShowUsername] = React.useState(false)
  const [showPassword, setShowPassword] = React.useState(false)
  const [testResult, setTestResult] = React.useState<ProxyResourceTestResponse | null>(null)
  const create = useCreateProxyResource()
  const update = useUpdateProxyResource()
  const setCredentialProxy = useSetCredentialProxy()
  const testProxy = useTestProxyResource()
  const credentials = useCredentials({ enabled: open })
  const isEditing = Boolean(resource)
  const allCredentials = credentials.data?.credentials || []

  React.useEffect(() => {
    if (!open) return
    let cancelled = false
    setForm(resource ? formFromResource(resource) : emptyForm())
    setSelectedIds(new Set())
    setBindingReady(false)
    setTestResult(null)
    setShowUsername(false)
    setShowPassword(false)
    credentials.refetch().then((result) => {
      if (cancelled || result.error || !result.data) return
      const next = result.data.credentials || []
      setSelectedIds(new Set(resource ? next.filter((c) => c.proxyResourceId === resource.id).map((c) => c.id) : []))
      setBindingReady(true)
    })
    return () => { cancelled = true }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, resource?.id])

  const set = (key: keyof ProxyForm, value: string | boolean) =>
    setForm((prev) => ({ ...prev, [key]: value }))

  const toggleCredential = (id: number) =>
    setSelectedIds((prev) => { const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next })

  const syncBindings = async (resourceId: number) => {
    const ops = allCredentials
      .map((c) => {
        const selected = selectedIds.has(c.id)
        if (selected && c.proxyResourceId !== resourceId) return { id: c.id, proxyResourceId: resourceId }
        if (isEditing && !selected && c.proxyResourceId === resourceId) return { id: c.id, proxyResourceId: null }
        return null
      })
      .filter((op): op is { id: number; proxyResourceId: number | null } => Boolean(op))
    if (!ops.length) return { ok: 0, fail: 0 }
    const results = await Promise.allSettled(
      ops.map((op) => setCredentialProxy.mutateAsync({ id: op.id, request: { proxyResourceId: op.proxyResourceId } }))
    )
    return { ok: results.filter((r) => r.status === 'fulfilled').length, fail: results.filter((r) => r.status === 'rejected').length }
  }

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!form.name.trim()) return toast.error('请输入代理名称')
    if (!form.proxyUrl.trim()) return toast.error('请输入代理 URL')
    if (!bindingReady) return toast.error('账号列表仍在加载，请稍后再保存')
    if (credentials.isError || !credentials.data) return toast.error(`账号列表加载失败，无法同步绑定: ${extractErrorMessage(credentials.error)}`)
    const request = {
      name: form.name.trim(), proxyUrl: form.proxyUrl.trim(),
      proxyUsername: form.proxyUsername.trim() || undefined,
      proxyPassword: form.proxyPassword.trim() || undefined,
      enabled: form.enabled,
      notes: form.notes.trim() || undefined,
      clearUsername: isEditing && !form.proxyUsername.trim(),
      clearPassword: isEditing && !form.proxyPassword.trim(),
      clearNotes: isEditing && !form.notes.trim(),
    }
    try {
      const saved = isEditing && resource
        ? await update.mutateAsync({ id: resource.id, request })
        : await create.mutateAsync(request)
      const binding = await syncBindings(saved.id)
      if (binding.fail > 0) toast.warning(`代理已保存，账号绑定成功 ${binding.ok} 个，失败 ${binding.fail} 个`)
      else if (binding.ok > 0) toast.success(`代理已保存，已同步 ${binding.ok} 个账号绑定`)
      else toast.success(isEditing ? '代理资源已更新' : '代理资源已创建')
      onClose()
    } catch (error) {
      toast.error(`${isEditing ? '更新' : '创建'}失败: ${extractErrorMessage(error)}`)
    }
  }

  const testCurrent = async () => {
    if (!form.proxyUrl.trim()) return toast.error('请输入代理 URL')
    try {
      const result = await testProxy.mutateAsync({
        request: { proxyUrl: form.proxyUrl.trim(), proxyUsername: form.proxyUsername.trim() || undefined, proxyPassword: form.proxyPassword.trim() || undefined },
      })
      setTestResult(result)
      if (result.success) toast.success(result.message)
      else toast.error(result.message)
    } catch (error) {
      toast.error(`代理测试失败: ${extractErrorMessage(error)}`)
    }
  }

  const pending = create.isPending || update.isPending || setCredentialProxy.isPending || testProxy.isPending

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={isEditing && resource ? `编辑代理：${resource.name}` : '新增代理 / 家宽'}
      width="max-w-3xl"
      footer={
        <>
          <Button variant="outline" size="sm" onClick={onClose} disabled={pending}>取消</Button>
          <Button size="sm" disabled={pending || !bindingReady} onClick={submit}>
            {pending ? <Spinner size="sm" /> : <Plus className="size-4" />}
            {isEditing ? '保存代理' : '新增代理'}
          </Button>
        </>
      }
    >
      <form className="space-y-4" onSubmit={submit}>
        <FieldGrid>
          <Field label="名称">
            <Input value={form.name} onChange={(e) => set('name', e.target.value)} placeholder="住宅家宽 A / US Proxy 1" />
          </Field>
          <Field label="代理 URL" description="支持 http://、https://、socks5://、socks5h://">
            <Input value={form.proxyUrl} onChange={(e) => set('proxyUrl', e.target.value)} placeholder="socks5h://127.0.0.1:1080" />
          </Field>
          <Field label="用户名">
            <SecretInput value={form.proxyUsername} onChange={(v) => set('proxyUsername', v)} visible={showUsername} onToggle={() => setShowUsername((v) => !v)} />
          </Field>
          <Field label="密码" description={isEditing ? '留空保存会清除当前密码' : undefined}>
            <SecretInput value={form.proxyPassword} onChange={(v) => set('proxyPassword', v)} visible={showPassword} onToggle={() => setShowPassword((v) => !v)} />
          </Field>
          <Field label="备注">
            <Textarea className="min-h-20" value={form.notes} onChange={(e) => set('notes', e.target.value)} />
          </Field>
          <div className="flex items-center justify-between gap-3 self-end rounded-xl border border-border bg-muted/40 p-3">
            <div>
              <div className="text-xs font-semibold">启用状态</div>
              <div className="text-xs text-muted-foreground">禁用后绑定该资源的账号不会回退到全局代理或直连。</div>
            </div>
            <Switch checked={form.enabled} onCheckedChange={() => set('enabled', !form.enabled)} />
          </div>
        </FieldGrid>

        <div className="space-y-2 rounded-xl border border-border bg-muted/30 p-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <div className="text-sm font-semibold">代理连通性</div>
              <div className="text-xs text-muted-foreground">保存前先用当前表单配置测试真实出网。</div>
            </div>
            <Button variant="ghost" size="sm" onClick={testCurrent} disabled={pending || !form.proxyUrl.trim()}>
              {testProxy.isPending ? <Spinner size="sm" /> : <Activity className="size-4" />}
              测试代理
            </Button>
          </div>
          <ProxyTestResult result={testResult} />
        </div>

        <div className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-sm font-semibold">绑定账号</div>
              <div className="text-xs text-muted-foreground">可直接选择要绑定到该代理资源的账号；已禁用账号仍可选择，但会用红色标识。</div>
            </div>
            <Badge tone="info">{selectedIds.size} 已选</Badge>
          </div>
          {!bindingReady && (credentials.isLoading || credentials.isFetching) ? (
            <LoadingState text="加载账号..." />
          ) : !bindingReady && credentials.isError ? (
            <div className="rounded-xl border border-destructive/30 bg-destructive/5 p-4 text-center text-sm text-destructive">
              账号列表加载失败：{extractErrorMessage(credentials.error)}
            </div>
          ) : (
            <CredentialBindingPicker credentials={allCredentials} selectedIds={selectedIds} onToggle={toggleCredential} />
          )}
        </div>
      </form>
    </ModalShell>
  )
}

// ============================================================================
// ProxyResourceCard
// ============================================================================

function maskSecret(value?: string | null): string {
  if (!value) return '-'
  return '*'.repeat(Math.min(Math.max(value.length, 6), 16))
}

function Detail({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-muted-foreground">{label}</div>
      <div>{children}</div>
    </div>
  )
}

export function ProxyResourceCard({ resource, onEdit }: { resource: ProxyResource; onEdit: (r: ProxyResource) => void }) {
  const [showSecrets, setShowSecrets] = React.useState(false)
  const [testResult, setTestResult] = React.useState<ProxyResourceTestResponse | null>(null)
  const [expanded, setExpanded] = React.useState(false)
  const [selectedIds, setSelectedIds] = React.useState<Set<number>>(new Set())
  const [bindingReady, setBindingReady] = React.useState(false)
  const [savingBindings, setSavingBindings] = React.useState(false)
  const update = useUpdateProxyResource()
  const remove = useDeleteProxyResource()
  const testProxy = useTestProxyResource()
  const setCredentialProxy = useSetCredentialProxy()
  const confirm = useConfirm()
  const credentials = useCredentials({ enabled: expanded })

  const allCredentials = credentials.data?.credentials || []

  // 当展开时加载账号并初始化选中状态
  React.useEffect(() => {
    if (!expanded) return
    let cancelled = false
    setBindingReady(false)
    credentials.refetch().then((result) => {
      if (cancelled || result.error || !result.data) return
      const list = result.data.credentials || []
      setSelectedIds(new Set(list.filter((c) => c.proxyResourceId === resource.id).map((c) => c.id)))
      setBindingReady(true)
    })
    return () => { cancelled = true }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expanded, resource.id])

  const toggleCredential = (id: number) =>
    setSelectedIds((prev) => { const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next })

  const saveBindings = async () => {
    if (!bindingReady || !credentials.data) return toast.error('账号列表尚未加载完成')
    setSavingBindings(true)
    try {
      const ops = allCredentials
        .map((c) => {
          const selected = selectedIds.has(c.id)
          if (selected && c.proxyResourceId !== resource.id) return { id: c.id, proxyResourceId: resource.id }
          if (!selected && c.proxyResourceId === resource.id) return { id: c.id, proxyResourceId: null as null }
          return null
        })
        .filter((op): op is { id: number; proxyResourceId: number | null } => Boolean(op))
      if (!ops.length) { toast.success('绑定关系无变化'); return }
      const results = await Promise.allSettled(
        ops.map((op) => setCredentialProxy.mutateAsync({ id: op.id, request: { proxyResourceId: op.proxyResourceId } }))
      )
      const ok = results.filter((r) => r.status === 'fulfilled').length
      const fail = results.filter((r) => r.status === 'rejected').length
      if (fail > 0) toast.warning(`绑定成功 ${ok} 个，失败 ${fail} 个`)
      else toast.success(`已同步 ${ok} 个账号绑定`)
    } catch (error) {
      toast.error(`保存绑定失败: ${extractErrorMessage(error)}`)
    } finally {
      setSavingBindings(false)
    }
  }

  const toggleEnabled = () => update.mutate(
    { id: resource.id, request: { enabled: !resource.enabled } },
    {
      onSuccess: () => toast.success(resource.enabled ? '代理已禁用' : '代理已启用'),
      onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
    }
  )

  const testSavedProxy = async () => {
    try {
      const result = await testProxy.mutateAsync({
        id: resource.id,
        request: { proxyUrl: resource.proxyUrl, proxyUsername: resource.proxyUsername || undefined, proxyPassword: resource.proxyPassword || undefined },
      })
      setTestResult(result)
      if (result.success) toast.success(result.message)
      else toast.error(result.message)
    } catch (error) {
      toast.error(`代理测试失败: ${extractErrorMessage(error)}`)
    }
  }

  const deleteResource = async () => {
    const confirmed = await confirm({
      title: '删除代理资源',
      message: resource.credentialCount > 0
        ? `代理「${resource.name}」当前绑定 ${resource.credentialCount} 个账号，删除后这些账号会回退到全局代理或直连。确认删除？`
        : `确认删除代理资源「${resource.name}」？`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!confirmed) return
    remove.mutate(resource.id, {
      onSuccess: () => toast.success('代理资源已删除'),
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <div className="rounded-lg border border-border bg-card">
      <div className="p-4">
        <div className="flex flex-col gap-3">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-1.5">
                <h3 className="truncate text-sm font-semibold" title={resource.name}>{resource.name}</h3>
                <Badge>#{resource.id}</Badge>
                <Badge tone={resource.enabled ? 'success' : 'error'}>{resource.enabled ? '启用' : '已禁用'}</Badge>
                {resource.hasPassword && <Badge tone="info">密码</Badge>}
              </div>
              <div className="mt-1 truncate text-xs text-muted-foreground" title={resource.proxyUrl}>{resource.proxyUrl}</div>
            </div>
            <Switch checked={resource.enabled} disabled={update.isPending} onCheckedChange={toggleEnabled} />
          </div>

          <div className="grid gap-2 text-xs sm:grid-cols-2">
            <Detail label="用户名">
              <span className="font-mono font-semibold">{showSecrets ? resource.proxyUsername || '-' : maskSecret(resource.proxyUsername)}</span>
            </Detail>
            <Detail label="密码">
              <span className="font-mono font-semibold">{showSecrets ? resource.proxyPassword || '-' : resource.hasPassword ? maskSecret(resource.proxyPassword || '******') : '-'}</span>
            </Detail>
            <Detail label="绑定账号"><span className="font-semibold">{resource.credentialCount}</span></Detail>
            <Detail label="创建时间"><span className="font-semibold">{formatDate(resource.createdAt)}</span></Detail>
            <Detail label="更新时间"><span className="font-semibold">{formatDate(resource.updatedAt)}</span></Detail>
            {resource.notes && (
              <div className="sm:col-span-2">
                <div className="text-muted-foreground">备注</div>
                <div className="whitespace-pre-wrap break-words font-semibold">{resource.notes}</div>
              </div>
            )}
          </div>

          <ProxyTestResult result={testResult} />

          <div className="flex flex-wrap gap-1.5">
            <Button variant="ghost" size="xs" onClick={testSavedProxy} disabled={testProxy.isPending}>
              {testProxy.isPending ? <Spinner size="sm" /> : <Activity className="size-3.5" />}测试
            </Button>
            <Button variant="ghost" size="xs" onClick={() => setShowSecrets((v) => !v)}>
              {showSecrets ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
              {showSecrets ? '隐藏账号密码' : '显示账号密码'}
            </Button>
            <Button variant="ghost" size="xs" onClick={() => onEdit(resource)}>
              <Edit3 className="size-3.5" />编辑
            </Button>
            <Button
              variant="ghost" size="xs"
              className="text-destructive hover:bg-destructive/10 hover:text-destructive"
              onClick={deleteResource}
              disabled={remove.isPending}
            >
              <Trash2 className="size-3.5" />删除
            </Button>
            <Button
              variant="ghost" size="xs"
              className={cn(expanded ? 'text-primary' : '')}
              onClick={() => setExpanded((v) => !v)}
            >
              <Users className="size-3.5" />账号绑定
              {expanded ? <ChevronUp className="size-3.5" /> : <ChevronDown className="size-3.5" />}
            </Button>
          </div>
        </div>
      </div>

      {/* 展开态：账号绑定选择器 */}
      {expanded && (
        <div className="border-t border-border p-4 space-y-3 animate-in fade-in-0 slide-in-from-top-2 duration-200">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-sm font-semibold">账号绑定</div>
              <div className="text-xs text-muted-foreground">勾选账号后点击保存绑定同步到服务器；已禁用账号以红色标识。</div>
            </div>
            <div className="flex items-center gap-2">
              <Badge tone="info">{selectedIds.size} 已选</Badge>
              <Button
                size="xs"
                disabled={!bindingReady || savingBindings || setCredentialProxy.isPending}
                onClick={saveBindings}
              >
                {savingBindings ? <Spinner size="sm" /> : null}
                保存绑定
              </Button>
            </div>
          </div>
          {!bindingReady && (credentials.isLoading || credentials.isFetching) ? (
            <LoadingState text="加载账号..." />
          ) : !bindingReady && credentials.isError ? (
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-center text-xs text-destructive">
              账号列表加载失败：{extractErrorMessage(credentials.error)}
            </div>
          ) : (
            <CredentialBindingPicker
              credentials={allCredentials}
              selectedIds={selectedIds}
              onToggle={toggleCredential}
            />
          )}
        </div>
      )}
    </div>
  )
}
