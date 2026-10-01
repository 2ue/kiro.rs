import { lazy, Suspense, type ComponentProps } from 'react'
import { Skeleton } from '@/components/ui/skeleton'

export type { SeriesDef } from './trend-chart-impl'
export { RankList } from './rank-list'

const Impl = lazy(() => import('./trend-chart-impl'))

/** 趋势图：Recharts 按需加载，加载期间显示等高骨架 */
export function TrendChart(props: ComponentProps<typeof Impl>) {
  return (
    <Suspense fallback={<Skeleton className="w-full" style={{ height: props.height ?? 220 }} />}>
      <Impl {...props} />
    </Suspense>
  )
}
