import * as React from 'react'
import { Activity, Edit3, Eye, EyeOff, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { formatDate } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useDeleteProxyResource,
  useProxyResources,
  useTestProxyResource,
  useUpdateProxyResource,
} from '@/hooks/use-credentials'
import type { ProxyResource, ProxyResourceTestResponse } from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  EmptyState,
  ErrorState,
  LoadingState,
  useConfirm,
} from '@/components/patterns'
import { Badge, Button, Card, Spinner, Switch } from '@/components/ui'
import { ProxyEditorModal, ProxyTestResult } from './proxy-editor-modal'

function maskSecret(value?: string | null): string {
  if (!value) return '-'
  return '*'.repeat(Math.min(Math.max(value.length, 6), 16))
}

function ProxyResourceCard({
  resource,
  onEdit,
}: {
  resource: ProxyResource
  onEdit: (resource: ProxyResource) => void
}) {
  const [showSecrets, setShowSecrets] = React.useState(false)
  const [testResult, setTestResult] = React.useState<ProxyResourceTestResponse | null>(null)
  const update = useUpdateProxyResource()
  const remove = useDeleteProxyResource()
  const testProxy = useTestProxyResource()
  const confirm = useConfirm()

  const toggleEnabled = () => {
    update.mutate(
      { id: resource.id, request: { enabled: !resource.enabled } },
      {
        onSuccess: () => toast.success(resource.enabled ? '代理已禁用' : '代理已启用'),
        onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const testSavedProxy = async () => {
    try {
      const result = await testProxy.mutateAsync({
        id: resource.id,
        request: {
          proxyUrl: resource.proxyUrl,
          proxyUsername: resource.proxyUsername || undefined,
          proxyPassword: resource.proxyPassword || undefined,
        },
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
      message:
        resource.credentialCount > 0
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
    <Card className="p-3">
      <div className="flex flex-col gap-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-1.5">
              <h3 className="truncate text-sm font-semibold" title={resource.name}>
                {resource.name}
              </h3>
              <Badge>#{resource.id}</Badge>
              <Badge tone={resource.enabled ? 'success' : 'error'}>
                {resource.enabled ? '启用' : '已禁用'}
              </Badge>
              {resource.hasPassword && <Badge tone="info">密码</Badge>}
            </div>
            <div className="mt-1 truncate text-xs text-muted-foreground" title={resource.proxyUrl}>
              {resource.proxyUrl}
            </div>
          </div>
          <Switch checked={resource.enabled} disabled={update.isPending} onCheckedChange={toggleEnabled} />
        </div>

        <div className="grid gap-2 text-xs sm:grid-cols-2">
          <Detail label="用户名">
            <span className="font-mono font-semibold">
              {showSecrets ? resource.proxyUsername || '-' : maskSecret(resource.proxyUsername)}
            </span>
          </Detail>
          <Detail label="密码">
            <span className="font-mono font-semibold">
              {showSecrets
                ? resource.proxyPassword || '-'
                : resource.hasPassword
                  ? maskSecret(resource.proxyPassword || '******')
                  : '-'}
            </span>
          </Detail>
          <Detail label="绑定账号">
            <span className="font-semibold">{resource.credentialCount}</span>
          </Detail>
          <Detail label="创建时间">
            <span className="font-semibold">{formatDate(resource.createdAt)}</span>
          </Detail>
          <Detail label="更新时间">
            <span className="font-semibold">{formatDate(resource.updatedAt)}</span>
          </Detail>
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
            {testProxy.isPending ? <Spinner size="sm" /> : <Activity className="size-3.5" />}
            测试
          </Button>
          <Button variant="ghost" size="xs" onClick={() => setShowSecrets((v) => !v)}>
            {showSecrets ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
            {showSecrets ? '隐藏账号密码' : '显示账号密码'}
          </Button>
          <Button variant="ghost" size="xs" onClick={() => onEdit(resource)}>
            <Edit3 className="size-3.5" />
            编辑
          </Button>
          <Button
            variant="ghost"
            size="xs"
            className="text-destructive hover:bg-destructive/10 hover:text-destructive"
            onClick={deleteResource}
            disabled={remove.isPending}
          >
            <Trash2 className="size-3.5" />
            删除
          </Button>
        </div>
      </div>
    </Card>
  )
}

function Detail({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-muted-foreground">{label}</div>
      <div>{children}</div>
    </div>
  )
}

export function ProxiesPage() {
  const resources = useProxyResources()
  const [editorOpen, setEditorOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<ProxyResource | null>(null)
  const list = resources.data?.resources || []
  const enabledCount = React.useMemo(() => list.filter((r) => r.enabled).length, [list])
  const boundCount = React.useMemo(
    () => list.reduce((sum, r) => sum + r.credentialCount, 0),
    [list]
  )

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

  return (
    <PageContainer>
      <PageHeader title={pageMeta.proxies.title} subtitle={pageMeta.proxies.subtitle} />

      <StatGrid>
        <StatCard title="代理资源" value={list.length} tone="info" />
        <StatCard title="启用资源" value={enabledCount} tone={enabledCount ? 'success' : 'warning'} />
        <StatCard title="绑定账号" value={boundCount} />
      </StatGrid>

      <SectionCard
        title="代理资源"
        actions={
          <>
            <Button size="sm" onClick={openCreate}>
              <Plus className="size-4" />
              新增代理
            </Button>
            <Button variant="outline" size="sm" onClick={() => resources.refetch()}>
              <RefreshCw className="size-4" />
              刷新列表
            </Button>
          </>
        }
      >
        {resources.isLoading ? (
          <LoadingState />
        ) : resources.error ? (
          <ErrorState message={extractErrorMessage(resources.error)} />
        ) : list.length === 0 ? (
          <EmptyState title="暂无代理资源" />
        ) : (
          <div className="grid gap-3 lg:grid-cols-2 xl:grid-cols-3">
            {list.map((resource) => (
              <ProxyResourceCard key={resource.id} resource={resource} onEdit={openEdit} />
            ))}
          </div>
        )}
      </SectionCard>

      <ProxyEditorModal open={editorOpen} resource={editing} onClose={closeEditor} />
    </PageContainer>
  )
}
