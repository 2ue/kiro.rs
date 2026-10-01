import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { Plus } from 'lucide-react'
import { usageApi } from '@/api/endpoints/usage'
import type { ExternalPool } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { EmptyState, ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { ToneBadge } from '@/components/status/tone-badge'
import { qk } from '@/queries/keys'
import { usePoolsStatus } from '@/queries/shared'
import { usePollInterval, useUiPrefs } from '@/stores/ui-prefs'
import { DirtyBar } from '@/features/settings/dirty-bar'
import { GroupCard } from '@/features/settings/field-renderer'
import { POOL_POLICY_GROUPS } from '@/features/settings/registry'
import { useConfigDraft } from '@/features/settings/use-config-draft'
import { PoolCard } from './pool-card'
import { PoolDetailSheet } from './pool-detail-sheet'
import { PoolFormSheet } from './pool-form-sheet'

const route = getRouteApi('/pools')

export function PoolsPage() {
  const search = route.useSearch()
  const navigate = route.useNavigate()
  const status = usePoolsStatus()
  const windowKey = useUiPrefs((s) => s.windowKey)
  const billing = useQuery({
    queryKey: qk.usage.poolBilling(windowKey),
    queryFn: () => usageApi.poolBilling(windowKey),
    refetchInterval: usePollInterval('normal'),
  })
  const [editing, setEditing] = useState<ExternalPool | 'new' | null>(null)
  const tab = search.tab ?? 'pools'
  const pools = status.data?.pools ?? []
  const billingOf = (id: number) => billing.data?.externalPoolBillingByPool.find((b) => b.poolId === id)
  const selected = pools.find((p) => p.pool.id === search.id)
  const dispatchable = pools.filter((p) => p.dispatchable).length

  return (
    <Page>
      <PageHeader
        title="外部池"
        description={
          status.data ? (
            <span className="num">
              {pools.length} 个外部池 · {dispatchable} 个可派发 · 作为本地账号的兜底或按策略直连
            </span>
          ) : (
            '第三方 Anthropic 兼容上游'
          )
        }
        actions={
          tab === 'pools' && (
            <Button size="sm" onClick={() => setEditing('new')}>
              <Plus /> 新建外部池
            </Button>
          )
        }
      />
      <Tabs
        value={tab}
        onValueChange={(v) =>
          navigate({ search: (p) => ({ ...p, tab: v === 'pools' ? undefined : (v as 'policy'), focus: undefined }), replace: true })
        }
      >
        <TabsList variant="line" className="border-b">
          <TabsTrigger value="pools">外部池</TabsTrigger>
          <TabsTrigger value="policy">路由策略</TabsTrigger>
        </TabsList>
        <TabsContent value="pools" className="pt-4">
          {status.isLoading ? (
            <LoadingRows rows={4} />
          ) : status.error && !status.data ? (
            <ErrorState error={status.error} onRetry={() => status.refetch()} />
          ) : pools.length === 0 ? (
            <EmptyState
              title="还没有外部池"
              description="外部池可以在本地账号不可用时兜底，或者按模型/路径直连"
              action={
                <Button size="sm" onClick={() => setEditing('new')}>
                  <Plus /> 新建外部池
                </Button>
              }
            />
          ) : (
            <div className="grid gap-3 md:grid-cols-2 2xl:grid-cols-3">
              {[...pools]
                .sort((a, b) => Number(b.pool.enabled) - Number(a.pool.enabled) || a.pool.priority - b.pool.priority)
                .map((p) => (
                  <PoolCard
                    key={p.pool.id}
                    status={p}
                    billing={billingOf(p.pool.id)}
                    active={search.id === p.pool.id}
                    onOpen={() => navigate({ search: (s) => ({ ...s, id: p.pool.id }) })}
                    onEdit={() => setEditing(p.pool)}
                  />
                ))}
            </div>
          )}
        </TabsContent>
        <TabsContent value="policy" className="pt-4">
          <PolicyTab focus={search.focus} />
        </TabsContent>
      </Tabs>

      <PoolDetailSheet
        status={selected}
        billing={selected ? billingOf(selected.pool.id) : undefined}
        onClose={() => navigate({ search: (s) => ({ ...s, id: undefined }), replace: true })}
        onEdit={() => selected && setEditing(selected.pool)}
      />
      <PoolFormSheet
        pool={editing && editing !== 'new' ? editing : undefined}
        open={editing !== null}
        onOpenChange={(o) => !o && setEditing(null)}
      />
    </Page>
  )
}

function PolicyTab({ focus }: { focus?: string }) {
  const cfg = useConfigDraft()
  if (!cfg.ready) return cfg.query.error ? <ErrorState error={cfg.query.error} /> : <LoadingRows rows={8} />
  const enabled = !!cfg.get('externalPools.externalPoolsEnabled')
  const direct = !!cfg.get('externalPools.externalDirectPolicyEnabled')
  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <Alert>
        <AlertDescription className="flex flex-wrap items-center gap-2">
          当前：
          <ToneBadge tone={enabled ? 'success' : 'neutral'}>{enabled ? '外部池已启用' : '外部池未启用'}</ToneBadge>
          {enabled && <ToneBadge tone={direct ? 'info' : 'neutral'}>{direct ? '直连策略开启' : '仅作为兜底'}</ToneBadge>}
          <span className="text-xs text-muted-foreground">这里是外部池路由策略的唯一编辑入口。</span>
        </AlertDescription>
      </Alert>
      {POOL_POLICY_GROUPS.map((g) => (
        <GroupCard key={g.id} group={g} cfg={cfg} focus={focus} />
      ))}
      <DirtyBar cfg={cfg} />
    </div>
  )
}
