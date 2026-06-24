import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function CredentialsPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.credentials.title} subtitle={pageMeta.credentials.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
