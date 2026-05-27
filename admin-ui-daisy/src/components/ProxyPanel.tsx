import { Edit3, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button, Card, Checkbox, Input, Loading, Modal, Toggle, Textarea } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, FieldLabel, LoadingState, ModalShell, SectionCard, StatCard } from '@/components/common'
import { formatDate } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useCreateProxyResource,
  useCredentials,
  useDeleteProxyResource,
  useProxyResources,
  useSetCredentialProxy,
  useUpdateProxyResource,
} from '@/hooks/use-credentials'
import type { CredentialStatusItem, ProxyResource } from '@/types/api'

type ProxyForm = {
  name: string
  proxyUrl: string
  proxyUsername: string
  proxyPassword: string
  notes: string
  enabled: boolean
}

function emptyForm(): ProxyForm {
  return {
    name: '',
    proxyUrl: '',
    proxyUsername: '',
    proxyPassword: '',
    notes: '',
    enabled: true,
  }
}

function formFromResource(resource: ProxyResource): ProxyForm {
  return {
    name: resource.name,
    proxyUrl: resource.proxyUrl,
    proxyUsername: resource.proxyUsername || '',
    proxyPassword: '',
    notes: resource.notes || '',
    enabled: resource.enabled,
  }
}

function credentialLabel(credential: CredentialStatusItem) {
  return credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
}

function CredentialBindingPicker({
  credentials,
  selectedIds,
  onToggle,
}: {
  credentials: CredentialStatusItem[]
  selectedIds: Set<number>
  onToggle: (id: number) => void
}) {
  if (!credentials.length) {
    return <EmptyState text="暂无可绑定凭据" />
  }

  const sortedCredentials = [...credentials].sort((left, right) => {
    if (left.disabled !== right.disabled) return left.disabled ? 1 : -1
    return left.id - right.id
  })

  return (
    <div className="max-h-72 space-y-2 overflow-y-auto rounded-box border border-base-300 bg-base-100 p-2">
      {sortedCredentials.map((credential) => {
        const selected = selectedIds.has(credential.id)
        return (
          <label
            key={credential.id}
            className={`flex cursor-pointer items-start gap-3 rounded-box border p-2 text-sm transition ${
              selected ? 'border-primary bg-primary/5' : credential.disabled ? 'border-error/25 bg-error/5 opacity-80' : 'border-base-300 bg-base-100 hover:bg-base-200'
            }`}
          >
            <Checkbox size="sm" className="mt-0.5" checked={selected} onChange={() => onToggle(credential.id)} />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="truncate font-semibold" title={credentialLabel(credential)}>{credentialLabel(credential)}</span>
                <Badge>#{credential.id}</Badge>
                <Badge tone={credential.disabled ? 'error' : 'success'}>{credential.disabled ? '已禁用' : '启用'}</Badge>
                {credential.proxyResourceName && <Badge tone="info">{credential.proxyResourceName}</Badge>}
              </div>
              <div className="mt-1 truncate text-xs text-base-content/55">
                {credential.effectiveProxyUrl || '当前未使用代理资源'}
              </div>
            </div>
          </label>
        )
      })}
    </div>
  )
}

function ProxyEditorModal({
  open,
  resource,
  onClose,
}: {
  open: boolean
  resource: ProxyResource | null
  onClose: () => void
}) {
  const [form, setForm] = useState(() => resource ? formFromResource(resource) : emptyForm())
  const [selectedCredentialIds, setSelectedCredentialIds] = useState<Set<number>>(new Set())
  const [bindingReady, setBindingReady] = useState(false)
  const create = useCreateProxyResource()
  const update = useUpdateProxyResource()
  const setCredentialProxy = useSetCredentialProxy()
  const credentials = useCredentials({ enabled: open })
  const isEditing = Boolean(resource)
  const allCredentials = credentials.data?.credentials || []

  useEffect(() => {
    if (!open) return
    let cancelled = false
    setForm(resource ? formFromResource(resource) : emptyForm())
    setSelectedCredentialIds(new Set())
    setBindingReady(false)
    credentials.refetch().then((result) => {
      if (cancelled || result.error || !result.data) return
      const nextCredentials = result.data.credentials || []
      setSelectedCredentialIds(new Set(resource ? nextCredentials.filter((credential) => credential.proxyResourceId === resource.id).map((credential) => credential.id) : []))
      setBindingReady(true)
    })
    return () => {
      cancelled = true
    }
  }, [open, resource?.id])

  const set = (key: keyof ProxyForm, value: string | boolean) => setForm((prev) => ({ ...prev, [key]: value }))

  const toggleCredential = (id: number) => {
    setSelectedCredentialIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const syncCredentialBindings = async (resourceId: number) => {
    const operations = allCredentials
      .map((credential) => {
        const selected = selectedCredentialIds.has(credential.id)
        if (selected && credential.proxyResourceId !== resourceId) {
          return { id: credential.id, proxyResourceId: resourceId }
        }
        if (isEditing && !selected && credential.proxyResourceId === resourceId) {
          return { id: credential.id, proxyResourceId: null }
        }
        return null
      })
      .filter((item): item is { id: number; proxyResourceId: number | null } => Boolean(item))

    if (!operations.length) return { ok: 0, fail: 0 }
    const results = await Promise.allSettled(
      operations.map((operation) =>
        setCredentialProxy.mutateAsync({
          id: operation.id,
          request: { proxyResourceId: operation.proxyResourceId },
        })
      )
    )
    return {
      ok: results.filter((result) => result.status === 'fulfilled').length,
      fail: results.filter((result) => result.status === 'rejected').length,
    }
  }

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!form.name.trim()) return toast.error('请输入代理名称')
    if (!form.proxyUrl.trim()) return toast.error('请输入代理 URL')
    if (!bindingReady || credentials.isLoading || credentials.isFetching) {
      return toast.error('凭据列表仍在加载，请稍后再保存')
    }
    if (credentials.isError || !credentials.data) {
      return toast.error(`凭据列表加载失败，无法同步绑定: ${extractErrorMessage(credentials.error)}`)
    }
    const request = {
      name: form.name.trim(),
      proxyUrl: form.proxyUrl.trim(),
      proxyUsername: form.proxyUsername.trim() || undefined,
      proxyPassword: form.proxyPassword.trim() || undefined,
      enabled: form.enabled,
      notes: form.notes.trim() || undefined,
      clearUsername: isEditing && !form.proxyUsername.trim(),
      clearNotes: isEditing && !form.notes.trim(),
    }

    try {
      const saved = isEditing && resource
        ? await update.mutateAsync({ id: resource.id, request })
        : await create.mutateAsync(request)
      const bindingResult = await syncCredentialBindings(saved.id)
      if (bindingResult.fail > 0) {
        toast.warning(`代理已保存，凭据绑定成功 ${bindingResult.ok} 个，失败 ${bindingResult.fail} 个`)
      } else if (bindingResult.ok > 0) {
        toast.success(`代理已保存，已同步 ${bindingResult.ok} 个凭据绑定`)
      } else {
        toast.success(isEditing ? '代理资源已更新' : '代理资源已创建')
      }
      onClose()
    } catch (error) {
      toast.error(`${isEditing ? '更新' : '创建'}失败: ${extractErrorMessage(error)}`)
    }
  }

  const pending = create.isPending || update.isPending || setCredentialProxy.isPending

  return (
    <ModalShell open={open} title={isEditing && resource ? `编辑代理：${resource.name}` : '新增代理 / 家宽'} width="max-w-4xl" onClose={onClose}>
      <form className="space-y-4" onSubmit={submit}>
        <div className="grid gap-3 md:grid-cols-2">
          <FieldLabel title="名称">
            <Input bordered size="sm" value={form.name} onChange={(event) => set('name', event.target.value)} placeholder="住宅家宽 A / US Proxy 1" />
          </FieldLabel>
          <FieldLabel title="代理 URL" description="支持 http://、https://、socks5://、socks5h://">
            <Input bordered size="sm" value={form.proxyUrl} onChange={(event) => set('proxyUrl', event.target.value)} placeholder="socks5h://127.0.0.1:1080" />
          </FieldLabel>
          <FieldLabel title="用户名">
            <Input bordered size="sm" value={form.proxyUsername} onChange={(event) => set('proxyUsername', event.target.value)} />
          </FieldLabel>
          <FieldLabel title="密码" description={isEditing ? '留空表示不修改密码' : undefined}>
            <Input bordered size="sm" type="password" value={form.proxyPassword} onChange={(event) => set('proxyPassword', event.target.value)} />
          </FieldLabel>
          <FieldLabel title="备注">
            <Textarea bordered size="sm" className="min-h-20" value={form.notes} onChange={(event) => set('notes', event.target.value)} />
          </FieldLabel>
          <div className="flex items-end justify-between gap-3 rounded-box border border-base-300 bg-base-200 p-3">
            <div>
              <div className="text-xs font-semibold">启用状态</div>
              <div className="text-xs text-base-content/60">禁用后绑定该资源的凭据不会回退到全局代理或直连。</div>
            </div>
            <Toggle color="primary" checked={form.enabled} onChange={() => set('enabled', !form.enabled)} />
          </div>
        </div>

        <div className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-sm font-semibold">绑定凭据</div>
              <div className="text-xs text-base-content/60">可直接选择要绑定到该代理资源的凭据；已禁用凭据仍可选择，但会用红色标识。</div>
            </div>
            <Badge tone="info">{selectedCredentialIds.size} 已选</Badge>
          </div>
          {!bindingReady && (credentials.isLoading || credentials.isFetching) ? (
            <LoadingState text="加载凭据..." />
          ) : !bindingReady && credentials.isError ? (
            <div className="rounded-box border border-error/30 bg-error/5 p-4 text-center text-sm text-error">
              凭据列表加载失败：{extractErrorMessage(credentials.error)}
            </div>
          ) : (
            <CredentialBindingPicker credentials={allCredentials} selectedIds={selectedCredentialIds} onToggle={toggleCredential} />
          )}
        </div>

        <Modal.Actions>
          <Button type="button" color="ghost" size="sm" onClick={onClose} disabled={pending}>
            取消
          </Button>
          <Button type="submit" color="primary" size="sm" disabled={pending || !bindingReady}>
            {pending ? <Loading size="sm" /> : <Plus className="h-4 w-4" />}
            {isEditing ? '保存代理' : '新增代理'}
          </Button>
        </Modal.Actions>
      </form>
    </ModalShell>
  )
}

function ProxyResourceCard({ resource, onEdit }: { resource: ProxyResource; onEdit: (resource: ProxyResource) => void }) {
  const update = useUpdateProxyResource()
  const remove = useDeleteProxyResource()

  const toggleEnabled = () => {
    update.mutate(
      { id: resource.id, request: { enabled: !resource.enabled } },
      {
        onSuccess: () => toast.success(resource.enabled ? '代理资源已禁用' : '代理资源已启用'),
        onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const deleteResource = () => {
    if (!confirm(`确定删除代理资源「${resource.name}」吗？如果仍有凭据绑定，后端会拒绝删除。`)) return
    remove.mutate(resource.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <Card className="credential-card">
      <Card.Body className="gap-3 p-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-1.5">
              <h3 className="truncate text-sm font-semibold" title={resource.name}>{resource.name}</h3>
              <Badge>#{resource.id}</Badge>
              <Badge tone={resource.enabled ? 'success' : 'error'}>{resource.enabled ? '启用' : '已禁用'}</Badge>
              {resource.hasPassword && <Badge tone="info">密码</Badge>}
            </div>
            <div className="mt-1 truncate text-xs text-base-content/60" title={resource.proxyUrl}>{resource.proxyUrl}</div>
          </div>
          <Toggle color="primary" size="sm" checked={resource.enabled} disabled={update.isPending} onChange={toggleEnabled} />
        </div>
        <div className="grid gap-2 text-xs md:grid-cols-2">
          <div>
            <div className="text-base-content/50">用户名</div>
            <div className="font-semibold">{resource.proxyUsername || '-'}</div>
          </div>
          <div>
            <div className="text-base-content/50">绑定凭据</div>
            <div className="font-semibold">{resource.credentialCount}</div>
          </div>
          <div>
            <div className="text-base-content/50">创建时间</div>
            <div className="font-semibold">{formatDate(resource.createdAt)}</div>
          </div>
          <div>
            <div className="text-base-content/50">更新时间</div>
            <div className="font-semibold">{formatDate(resource.updatedAt)}</div>
          </div>
          {resource.notes && (
            <div className="md:col-span-2">
              <div className="text-base-content/50">备注</div>
              <div className="whitespace-pre-wrap break-words font-semibold">{resource.notes}</div>
            </div>
          )}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button type="button" color="ghost" size="xs" onClick={() => onEdit(resource)}>
            <Edit3 className="h-3.5 w-3.5" />
            编辑
          </Button>
          <Button type="button" color="ghost" size="xs" className="text-error hover:bg-error/10" onClick={deleteResource}>
            <Trash2 className="h-3.5 w-3.5" />
            删除
          </Button>
        </div>
      </Card.Body>
    </Card>
  )
}

export function ProxyPanel() {
  const resources = useProxyResources()
  const [editorOpen, setEditorOpen] = useState(false)
  const [editing, setEditing] = useState<ProxyResource | null>(null)
  const list = resources.data?.resources || []
  const enabledCount = useMemo(() => list.filter((resource) => resource.enabled).length, [list])
  const boundCount = useMemo(() => list.reduce((sum, resource) => sum + resource.credentialCount, 0), [list])

  const openCreate = () => {
    setEditing(null)
    setEditorOpen(true)
  }

  const openEdit = (resource: ProxyResource) => {
    setEditing(resource)
    setEditorOpen(true)
  }

  const closeEditor = () => {
    setEditorOpen(false)
    setEditing(null)
  }

  if (resources.isLoading) return <LoadingState />
  if (resources.error) return <ErrorState text={extractErrorMessage(resources.error)} />

  return (
    <div className="space-y-4">
      <div className="metric-grid">
        <StatCard title="代理资源" value={list.length} tone="info" />
        <StatCard title="启用资源" value={enabledCount} tone={enabledCount ? 'success' : 'warning'} />
        <StatCard title="绑定凭据" value={boundCount} />
      </div>

      <SectionCard
        title="代理资源"
        actions={
          <>
            <Button type="button" color="primary" size="sm" onClick={openCreate}>
              <Plus className="h-4 w-4" />
              新增代理
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => resources.refetch()}>
              <RefreshCw className="h-4 w-4" />
              刷新列表
            </Button>
          </>
        }
      >
        {list.length === 0 ? (
          <EmptyState text="暂无代理资源" />
        ) : (
          <div className="grid gap-3 lg:grid-cols-2 xl:grid-cols-3">
            {list.map((resource) => (
              <ProxyResourceCard key={resource.id} resource={resource} onEdit={openEdit} />
            ))}
          </div>
        )}
      </SectionCard>

      <ProxyEditorModal open={editorOpen} resource={editing} onClose={closeEditor} />
    </div>
  )
}
