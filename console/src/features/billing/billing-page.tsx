import { getRouteApi } from '@tanstack/react-router'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { useUiPrefs, WINDOW_OPTIONS } from '@/stores/ui-prefs'
import { CostTab } from './cost-tab'
import { PoolsBillingTab } from './pools-tab'
import { QuotaTab } from './quota-tab'

const route = getRouteApi('/billing')

export function BillingPage() {
  const { tab = 'cost' } = route.useSearch()
  const navigate = route.useNavigate()
  const windowKey = useUiPrefs((s) => s.windowKey)
  return (
    <Page>
      <PageHeader
        title="成本与额度"
        description={`费用拆分、账号额度生命周期、外部池盈亏 · 统计窗口：${WINDOW_OPTIONS.find((w) => w.key === windowKey)?.label}`}
      />
      <Tabs value={tab} onValueChange={(v) => navigate({ search: { tab: v === 'cost' ? undefined : (v as 'quota') }, replace: true })}>
        <TabsList variant="line" className="border-b">
          <TabsTrigger value="cost">费用</TabsTrigger>
          <TabsTrigger value="quota">账号额度</TabsTrigger>
          <TabsTrigger value="pools">外部池盈亏</TabsTrigger>
        </TabsList>
        <TabsContent value="cost" className="pt-4">
          <CostTab />
        </TabsContent>
        <TabsContent value="quota" className="pt-4">
          <QuotaTab />
        </TabsContent>
        <TabsContent value="pools" className="pt-4">
          <PoolsBillingTab />
        </TabsContent>
      </Tabs>
    </Page>
  )
}
