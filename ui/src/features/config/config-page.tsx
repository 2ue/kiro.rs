import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function ConfigPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.config.title} subtitle={pageMeta.config.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
