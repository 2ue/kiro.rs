import { Edit3, Plus, RefreshCw, Router, Trash2 } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button, Card, Input, Loading, Toggle, Textarea } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, FieldLabel, LoadingState, SectionCard, StatCard } from '@/components/common'
import { formatDate } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useCreateProxyResource,
  useDeleteProxyResource,
  useProxyResources,
  useUpdateProxyResource,
} from '@/hooks/use-credentials'
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

function ProxyEditor({
  resource,
  onDone,
}: {
  resource?: ProxyResource | null
  onDone?: () => void
}) {
  const [form, setForm] = useState(() => resource ? formFromResource(resource) : emptyForm())
  const create = useCreateProxyResource()
  const update = useUpdateProxyResource()
  const isEditing = Boolean(resource)

  useEffect(() => {
    setForm(resource ? formFromResource(resource) : emptyForm())
  }, [resource?.id])

  const set = (key: keyof ProxyForm, value: string | boolean) => setForm((prev) => ({ ...prev, [key]: value }))

  const submit = (event: React.FormEvent) => {
    event.preventDefault()
    if (!form.name.trim()) return toast.error('请输入代理名称')
    if (!form.proxyUrl.trim()) return toast.error('请输入代理 URL')
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
            onDone?.()
          },
          onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
        }
      )
    } else {
      create.mutate(request, {
        onSuccess: () => {
          toast.success('代理资源已创建')
          setForm(emptyForm())
          onDone?.()
        },
        onError: (error) => toast.error(`创建失败: ${extractErrorMessage(error)}`),
      })
    }
  }

  const pending = create.isPending || update.isPending

  return (
    <form className="grid gap-3 md:grid-cols-2" onSubmit={submit}>
      <FieldLabel title="名称">
        <Input bordered size="sm" value={form.name} onChange={(event) => set('name', event.target.value)} placeholder="住宅家宽 A / US Proxy 1" />
      </FieldLabel>
      <FieldLabel title="代理 URL" description="支持 http://、https://、socks5://">
        <Input bordered size="sm" value={form.proxyUrl} onChange={(event) => set('proxyUrl', event.target.value)} placeholder="socks5://127.0.0.1:1080" />
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
          <div className="text-xs text-base-content/60">禁用后绑定该资源的凭据会回退到全局代理或直连。</div>
        </div>
        <Toggle color="primary" checked={form.enabled} onChange={() => set('enabled', !form.enabled)} />
      </div>
      <div className="flex gap-2 md:col-span-2">
        <Button type="submit" color="primary" size="sm" disabled={pending}>
          {pending ? <Loading size="sm" /> : <Plus className="h-4 w-4" />}
          {isEditing ? '保存代理' : '新增代理'}
        </Button>
        {isEditing && (
          <Button type="button" color="ghost" size="sm" onClick={onDone} disabled={pending}>
            取消
          </Button>
        )}
      </div>
    </form>
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
    if (!confirm(`确定删除代理资源「${resource.name}」吗？绑定关系会被清除。`)) return
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
  const [editing, setEditing] = useState<ProxyResource | null>(null)
  const list = resources.data?.resources || []
  const enabledCount = useMemo(() => list.filter((resource) => resource.enabled).length, [list])
  const boundCount = useMemo(() => list.reduce((sum, resource) => sum + resource.credentialCount, 0), [list])

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
        title={editing ? `编辑代理：${editing.name}` : '新增代理 / 家宽'}
        description="这里统一维护稳定出口 IP，凭据可在卡片或添加时绑定；凭据直接代理 URL 会优先于绑定资源。"
      >
        <ProxyEditor resource={editing} onDone={() => setEditing(null)} />
      </SectionCard>

      <SectionCard
        title="代理资源"
        actions={
          <Button type="button" variant="outline" size="sm" onClick={() => resources.refetch()}>
            <RefreshCw className="h-4 w-4" />
            刷新列表
          </Button>
        }
      >
        {list.length === 0 ? (
          <EmptyState text="暂无代理资源" />
        ) : (
          <div className="grid gap-3 lg:grid-cols-2 xl:grid-cols-3">
            {list.map((resource) => (
              <ProxyResourceCard key={resource.id} resource={resource} onEdit={setEditing} />
            ))}
          </div>
        )}
      </SectionCard>

      <div className="hidden">
        <Router className="h-4 w-4" />
      </div>
    </div>
  )
}
