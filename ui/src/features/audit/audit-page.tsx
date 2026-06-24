import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function AuditPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.audit.title} subtitle={pageMeta.audit.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
