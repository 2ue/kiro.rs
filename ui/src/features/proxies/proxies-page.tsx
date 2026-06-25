import * as React from 'react'
import { Network, Plus, RefreshCw } from 'lucide-react'
import { useProxyResources } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { ProxyResource } from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
} from '@/components/patterns'
import { Button } from '@/components/ui'
import { ProxyResourceCard, ProxyEditorModal } from './proxy-components'

export function ProxiesPage() {
  const resources = useProxyResources()
  const [editorOpen, setEditorOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<ProxyResource | null>(null)
  const list = resources.data?.resources || []

  const enabledCount = React.useMemo(() => list.filter((r) => r.enabled).length, [list])
  const boundCount = React.useMemo(() => list.reduce((sum, r) => sum + r.credentialCount, 0), [list])

  const openCreate = () => { setEditing(null); setEditorOpen(true) }
  const openEdit = (resource: ProxyResource) => { setEditing(resource); setEditorOpen(true) }
  const closeEditor = () => { setEditorOpen(false); setEditing(null) }

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.proxies.title}
        subtitle={pageMeta.proxies.subtitle}
        actions={
          <div className="flex items-center gap-1.5">
            <Button variant="outline" size="sm" onClick={() => resources.refetch()}>
              <RefreshCw className={`h-4 w-4 ${resources.isFetching ? 'animate-spin' : ''}`} />
            </Button>
            <Button size="sm" onClick={openCreate}>
              <Plus className="h-4 w-4" />新增代理
            </Button>
          </div>
        }
      />

      <StatGrid>
        <StatCard title="代理资源" value={list.length} icon={<Network className="h-5 w-5" />} tone="info" />
        <StatCard title="启用资源" value={enabledCount} tone={enabledCount > 0 ? 'success' : 'warning'} />
        <StatCard title="绑定账号" value={boundCount} tone="default" />
      </StatGrid>

      <SectionCard
        title="代理资源"
        description={`共 ${list.length} 个代理资源`}
      >
        {resources.isLoading ? (
          <LoadingState text="加载代理资源..." />
        ) : resources.error ? (
          <ErrorState message={extractErrorMessage(resources.error)} />
        ) : list.length === 0 ? (
          <EmptyState
            icon={<Network className="h-12 w-12" />}
            title="暂无代理资源"
            description="点击右上角按钮创建第一个代理资源"
            action={
              <Button size="sm" onClick={openCreate}>
                <Plus className="h-4 w-4" />新增代理
              </Button>
            }
          />
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
