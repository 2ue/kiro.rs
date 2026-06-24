import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function DashboardPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.dashboard.title} subtitle={pageMeta.dashboard.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
