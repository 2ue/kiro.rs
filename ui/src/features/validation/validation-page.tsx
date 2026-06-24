import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function ValidationPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.validation.title} subtitle={pageMeta.validation.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
