import { PageContainer, PageHeader, EmptyState } from '@/components/patterns'
import { pageMeta } from '@/types/ui'

export function PricingPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.pricing.title} subtitle={pageMeta.pricing.subtitle} />
      <EmptyState title="开发中" description="该页面即将上线" />
    </PageContainer>
  )
}
