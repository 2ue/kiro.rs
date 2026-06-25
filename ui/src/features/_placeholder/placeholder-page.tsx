import { Construction } from 'lucide-react'
import { PageContainer, PageHeader } from '@/components/patterns'
import { pageMeta, type PageKey } from '@/types/ui'

/** 占位页:重构铺开过程中尚未实现的页面 */
export function PlaceholderPage({ pageKey }: { pageKey: PageKey }) {
  const meta = pageMeta[pageKey]
  return (
    <PageContainer>
      <PageHeader title={meta.title} subtitle={meta.subtitle} />
      <div className="flex flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border bg-surface-subtle/50 py-20 text-center">
        <Construction className="size-8 text-muted-foreground/60" />
        <div className="text-sm font-medium text-foreground/80">「{meta.title}」正在重构中</div>
        <div className="max-w-sm text-xs text-muted-foreground">
          该页面将在垂直切片验收通过后铺开实现。
        </div>
      </div>
    </PageContainer>
  )
}
