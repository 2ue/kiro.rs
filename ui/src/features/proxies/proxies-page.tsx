import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function ProxiesPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.proxies.title} subtitle={pageMeta.proxies.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
