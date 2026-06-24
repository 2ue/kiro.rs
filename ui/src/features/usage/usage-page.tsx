import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function UsagePage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.usage.title} subtitle={pageMeta.usage.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
