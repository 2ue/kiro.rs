import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function ExternalPoolsPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.external.title} subtitle={pageMeta.external.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
