import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Edit3, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  useCreateProxyResource,
  useCredentials,
  useDeleteProxyResource,
  useProxyResources,
  useSetCredentialProxy,
  useUpdateProxyResource,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
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

function formatDateTime(value: string | null): string {
  if (!value) return '未知'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
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
    return <Card><CardContent className="py-8 text-center text-sm text-muted-foreground">暂无可绑定凭据</CardContent></Card>
  }

  const sortedCredentials = [...credentials].sort((left, right) => {
    if (left.disabled !== right.disabled) return left.disabled ? 1 : -1
    return left.id - right.id
  })

  return (
    <div className="max-h-72 space-y-2 overflow-y-auto rounded-md border bg-background p-2">
      {sortedCredentials.map((credential) => {
        const selected = selectedIds.has(credential.id)
        return (
          <label
            key={credential.id}
            className={`flex cursor-pointer items-start gap-3 rounded-md border p-3 text-sm transition ${
              selected ? 'border-primary bg-primary/5' : credential.disabled ? 'border-red-200 bg-red-50 opacity-80 dark:border-red-900 dark:bg-red-950/30' : 'border-border hover:bg-muted/50'
            }`}
          >
            <Checkbox checked={selected} onCheckedChange={() => onToggle(credential.id)} className="mt-0.5" />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="truncate font-medium" title={credentialLabel(credential)}>{credentialLabel(credential)}</span>
                <Badge variant="outline">#{credential.id}</Badge>
                <Badge variant={credential.disabled ? 'destructive' : 'success'}>{credential.disabled ? '已禁用' : '启用'}</Badge>
                {credential.proxyResourceName && <Badge variant="outline">{credential.proxyResourceName}</Badge>}
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

function ProxyEditorDialog({
  open,
  resource,
  onOpenChange,
}: {
  open: boolean
  resource: ProxyResource | null
  onOpenChange: (open: boolean) => void
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

  const set = (key: keyof ProxyForm, value: string | boolean) => {
    setForm((prev) => ({ ...prev, [key]: value }))
  }

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
    if (!form.name.trim()) {
      toast.error('请输入代理名称')
      return
    }
    if (!form.proxyUrl.trim()) {
      toast.error('请输入代理 URL')
      return
    }
    if (!bindingReady) {
      toast.error('凭据列表仍在加载，请稍后再保存')
      return
    }
    if (credentials.isError || !credentials.data) {
      toast.error(`凭据列表加载失败，无法同步绑定: ${extractErrorMessage(credentials.error)}`)
      return
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
      onOpenChange(false)
    } catch (error) {
      toast.error(`${isEditing ? '更新' : '创建'}失败: ${extractErrorMessage(error)}`)
    }
  }

  const pending = create.isPending || update.isPending || setCredentialProxy.isPending

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => !pending && onOpenChange(nextOpen)}>
      <DialogContent className="flex max-h-[88vh] max-w-4xl flex-col overflow-hidden">
        <DialogHeader>
          <DialogTitle>{isEditing && resource ? `编辑代理：${resource.name}` : '新增代理 / 家宽'}</DialogTitle>
          <DialogDescription>维护稳定出口 IP，并直接选择需要绑定该代理资源的凭据。</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto pr-1">
          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <label className="text-sm font-medium">名称</label>
              <Input value={form.name} onChange={(e) => set('name', e.target.value)} placeholder="住宅家宽 A / US Proxy 1" />
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">代理 URL</label>
              <Input value={form.proxyUrl} onChange={(e) => set('proxyUrl', e.target.value)} placeholder="socks5h://127.0.0.1:1080" />
              <p className="text-xs text-muted-foreground">支持 http://、https://、socks5://、socks5h://</p>
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">用户名</label>
              <Input value={form.proxyUsername} onChange={(e) => set('proxyUsername', e.target.value)} />
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">密码</label>
              <Input type="password" value={form.proxyPassword} onChange={(e) => set('proxyPassword', e.target.value)} />
              {isEditing && <p className="text-xs text-muted-foreground">留空表示不修改密码</p>}
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">备注</label>
              <textarea
                value={form.notes}
                onChange={(e) => set('notes', e.target.value)}
                className="min-h-24 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
              />
            </div>
            <div className="flex items-center justify-between rounded-md border bg-muted/30 p-4">
              <div>
                <div className="text-sm font-medium">启用状态</div>
                <div className="text-xs text-muted-foreground">禁用后绑定该资源的凭据不会回退到全局代理或直连。</div>
              </div>
              <Switch checked={form.enabled} onCheckedChange={(checked) => set('enabled', checked)} />
            </div>
          </div>

          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="text-sm font-medium">绑定凭据</div>
                <div className="text-xs text-muted-foreground">已禁用凭据也可选择，会用红色样式区分。</div>
              </div>
              <Badge variant="outline">{selectedCredentialIds.size} 已选</Badge>
            </div>
            {!bindingReady && (credentials.isLoading || credentials.isFetching) ? (
              <div className="py-8 text-center text-sm text-muted-foreground">
                <div className="mx-auto mb-3 h-8 w-8 animate-spin rounded-full border-b-2 border-primary" />
                加载凭据...
              </div>
            ) : !bindingReady && credentials.isError ? (
              <Card>
                <CardContent className="py-8 text-center text-sm text-destructive">
                  凭据列表加载失败：{extractErrorMessage(credentials.error)}
                </CardContent>
              </Card>
            ) : (
              <CredentialBindingPicker credentials={allCredentials} selectedIds={selectedCredentialIds} onToggle={toggleCredential} />
            )}
          </div>

          <DialogFooter className="sticky bottom-0 bg-background pt-2">
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              取消
            </Button>
            <Button type="submit" disabled={pending || !bindingReady}>
              <Plus className="h-4 w-4" />
              {isEditing ? '保存代理' : '新增代理'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function ProxyResourceCard({
  resource,
  onEdit,
}: {
  resource: ProxyResource
  onEdit: (resource: ProxyResource) => void
}) {
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
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-start justify-between gap-3">
          <CardTitle className="min-w-0 text-base">
            <span className="block truncate">{resource.name}</span>
            <span className="mt-1 block truncate text-xs font-normal text-muted-foreground">{resource.proxyUrl}</span>
          </CardTitle>
          <Switch checked={resource.enabled} onCheckedChange={toggleEnabled} disabled={update.isPending} />
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex flex-wrap gap-2">
          <Badge variant="outline">#{resource.id}</Badge>
          <Badge variant={resource.enabled ? 'success' : 'destructive'}>{resource.enabled ? '启用' : '已禁用'}</Badge>
          {resource.hasPassword && <Badge variant="outline">密码</Badge>}
        </div>
        <div className="grid gap-2 text-sm md:grid-cols-2">
          <div>
            <span className="text-muted-foreground">用户名：</span>
            <span className="font-medium">{resource.proxyUsername || '-'}</span>
          </div>
          <div>
            <span className="text-muted-foreground">绑定凭据：</span>
            <span className="font-medium">{resource.credentialCount}</span>
          </div>
          <div>
            <span className="text-muted-foreground">创建：</span>
            <span className="font-medium">{formatDateTime(resource.createdAt)}</span>
          </div>
          <div>
            <span className="text-muted-foreground">更新：</span>
            <span className="font-medium">{formatDateTime(resource.updatedAt)}</span>
          </div>
          {resource.notes && (
            <div className="md:col-span-2">
              <span className="text-muted-foreground">备注：</span>
              <span className="whitespace-pre-wrap break-words font-medium">{resource.notes}</span>
            </div>
          )}
        </div>
        <div className="flex flex-wrap gap-2 border-t pt-3">
          <Button size="sm" variant="outline" onClick={() => onEdit(resource)}>
            <Edit3 className="h-4 w-4" />
            编辑
          </Button>
          <Button size="sm" variant="destructive" onClick={deleteResource}>
            <Trash2 className="h-4 w-4" />
            删除
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}

export function ProxyResourcesPanel() {
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

  const closeEditor = (open: boolean) => {
    setEditorOpen(open)
    if (!open) setEditing(null)
  }

  if (resources.isLoading) {
    return (
      <div className="py-12 text-center text-muted-foreground">
        <div className="mx-auto mb-4 h-10 w-10 animate-spin rounded-full border-b-2 border-primary" />
        加载代理资源...
      </div>
    )
  }

  if (resources.error) {
    return (
      <Card>
        <CardContent className="pt-6 text-center">
          <div className="mb-2 text-destructive">加载代理资源失败</div>
          <div className="mb-4 text-sm text-muted-foreground">{extractErrorMessage(resources.error)}</div>
          <Button onClick={() => resources.refetch()}>重试</Button>
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="space-y-6">
      <div className="grid gap-4 md:grid-cols-3">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">代理资源</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{list.length}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">启用资源</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold text-green-600">{enabledCount}</div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">绑定凭据</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{boundCount}</div>
          </CardContent>
        </Card>
      </div>

      <div className="flex items-center justify-between">
        <h2 className="text-xl font-semibold">代理资源</h2>
        <div className="flex gap-2">
          <Button size="sm" onClick={openCreate}>
            <Plus className="h-4 w-4" />
            新增代理
          </Button>
          <Button size="sm" variant="outline" onClick={() => resources.refetch()}>
            <RefreshCw className="h-4 w-4" />
            刷新列表
          </Button>
        </div>
      </div>

      {list.length === 0 ? (
        <Card>
          <CardContent className="py-8 text-center text-muted-foreground">暂无代理资源</CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {list.map((resource) => (
            <ProxyResourceCard key={resource.id} resource={resource} onEdit={openEdit} />
          ))}
        </div>
      )}

      <ProxyEditorDialog open={editorOpen} resource={editing} onOpenChange={closeEditor} />
    </div>
  )
}
