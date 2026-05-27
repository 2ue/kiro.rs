import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Edit3, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import {
  useCreateProxyResource,
  useDeleteProxyResource,
  useProxyResources,
  useUpdateProxyResource,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { ProxyResource } from '@/types/api'

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

function ProxyEditor({
  resource,
  onDone,
}: {
  resource: ProxyResource | null
  onDone: () => void
}) {
  const [form, setForm] = useState(() => resource ? formFromResource(resource) : emptyForm())
  const create = useCreateProxyResource()
  const update = useUpdateProxyResource()
  const isEditing = Boolean(resource)

  useEffect(() => {
    setForm(resource ? formFromResource(resource) : emptyForm())
  }, [resource?.id])

  const set = (key: keyof ProxyForm, value: string | boolean) => {
    setForm((prev) => ({ ...prev, [key]: value }))
  }

  const submit = (event: React.FormEvent) => {
    event.preventDefault()
    if (!form.name.trim()) {
      toast.error('请输入代理名称')
      return
    }
    if (!form.proxyUrl.trim()) {
      toast.error('请输入代理 URL')
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

    if (isEditing && resource) {
      update.mutate(
        { id: resource.id, request },
        {
          onSuccess: () => {
            toast.success('代理资源已更新')
            onDone()
          },
          onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
        }
      )
    } else {
      create.mutate(request, {
        onSuccess: () => {
          toast.success('代理资源已创建')
          setForm(emptyForm())
          onDone()
        },
        onError: (error) => toast.error(`创建失败: ${extractErrorMessage(error)}`),
      })
    }
  }

  const pending = create.isPending || update.isPending

  return (
    <form onSubmit={submit} className="grid gap-4 md:grid-cols-2">
      <div className="space-y-2">
        <label className="text-sm font-medium">名称</label>
        <Input value={form.name} onChange={(e) => set('name', e.target.value)} placeholder="住宅家宽 A / US Proxy 1" />
      </div>
      <div className="space-y-2">
        <label className="text-sm font-medium">代理 URL</label>
        <Input value={form.proxyUrl} onChange={(e) => set('proxyUrl', e.target.value)} placeholder="socks5://127.0.0.1:1080" />
        <p className="text-xs text-muted-foreground">支持 http://、https://、socks5://</p>
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
          <div className="text-xs text-muted-foreground">禁用后绑定该资源的凭据会回退到全局代理或直连。</div>
        </div>
        <Switch checked={form.enabled} onCheckedChange={(checked) => set('enabled', checked)} />
      </div>
      <div className="flex gap-2 md:col-span-2">
        <Button type="submit" disabled={pending}>
          <Plus className="h-4 w-4" />
          {isEditing ? '保存代理' : '新增代理'}
        </Button>
        {isEditing && (
          <Button type="button" variant="outline" onClick={onDone} disabled={pending}>
            取消
          </Button>
        )}
      </div>
    </form>
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
    if (!confirm(`确定删除代理资源「${resource.name}」吗？绑定关系会被清除。`)) return
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
  const [editing, setEditing] = useState<ProxyResource | null>(null)
  const list = resources.data?.resources || []
  const enabledCount = useMemo(() => list.filter((resource) => resource.enabled).length, [list])
  const boundCount = useMemo(() => list.reduce((sum, resource) => sum + resource.credentialCount, 0), [list])

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

      <Card>
        <CardHeader>
          <CardTitle>{editing ? `编辑代理：${editing.name}` : '新增代理 / 家宽'}</CardTitle>
          <p className="text-sm text-muted-foreground">
            这里统一维护稳定出口 IP，凭据可在卡片或添加时绑定；凭据直接代理 URL 会优先于绑定资源。
          </p>
        </CardHeader>
        <CardContent>
          <ProxyEditor resource={editing} onDone={() => setEditing(null)} />
        </CardContent>
      </Card>

      <div className="flex items-center justify-between">
        <h2 className="text-xl font-semibold">代理资源</h2>
        <Button size="sm" variant="outline" onClick={() => resources.refetch()}>
          <RefreshCw className="h-4 w-4" />
          刷新列表
        </Button>
      </div>

      {list.length === 0 ? (
        <Card>
          <CardContent className="py-8 text-center text-muted-foreground">暂无代理资源</CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {list.map((resource) => (
            <ProxyResourceCard key={resource.id} resource={resource} onEdit={setEditing} />
          ))}
        </div>
      )}
    </div>
  )
}
