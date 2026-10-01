import { useMemo } from 'react'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import type { ColumnDef } from '@tanstack/react-table'
import { RefreshCw } from 'lucide-react'
import { credentialsApi } from '@/api/endpoints/credentials'
import { Button } from '@/components/ui/button'
import { DataTable } from '@/components/data-table/data-table'
import { KpiCard } from '@/components/patterns/kpi-card'
import { EmptyState, LoadingRows } from '@/components/patterns/data-state'
import { Section } from '@/components/patterns/page-header'
import { QuotaBar } from '@/components/status/quota-bar'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtCompact, fmtDateTime, fmtInt, fmtPct, fmtRelative, fmtUsd } from '@/lib/format'
import { qk } from '@/queries/keys'
import { usePollInterval } from '@/stores/ui-prefs'
import { useAccountActions } from '@/features/accounts/actions'
import { toAccountRow, useCreditSummary, type AccountRow } from '@/features/accounts/queries'

const WEEK = 7 * 86400e3

/** 额度生命周期：即将耗尽、即将重置、超额中、快照过期 */
export function QuotaTab() {
  const credit = useCreditSummary()
  const actions = useAccountActions()
  const navigate = useNavigate()
  const query = { page: 1, limit: 500, status: 'enabled', sortBy: 'usage_percentage' as const, sortOrder: 'desc' as const }
  const page = useQuery({
    queryKey: qk.credentials.page(query),
    queryFn: () => credentialsApi.page(query),
    refetchInterval: usePollInterval('slow'),
    placeholderData: keepPreviousData,
  })
  const rows = useMemo(() => (page.data?.credentials ?? []).map(toAccountRow), [page.data])
  const now = page.dataUpdatedAt
  const nearlyOut = rows.filter((r) => r.quota.level === 'high' || r.quota.level === 'over')
  const resetting = rows.filter((r) => r.quota.nextResetAt && r.quota.nextResetAt.getTime() - now < WEEK).sort((a, b) => a.quota.nextResetAt!.getTime() - b.quota.nextResetAt!.getTime())
  const overage = rows.filter((r) => r.quota.overageEnabled || r.quota.overageUsd > 0)
  const stale = rows.filter((r) => r.quota.stale)

  const c = credit.data
  const columns: ColumnDef<AccountRow, unknown>[] = [
    { id: 'a', header: '账号', size: 200, meta: { grow: true }, cell: ({ row: { original: r } }) => <span className="truncate">{r.label}</span> },
    { id: 'q', header: '使用率', size: 160, cell: ({ row: { original: r } }) => <QuotaBar quota={r.quota} /> },
    {
      id: 'u',
      header: '已用 / 总量',
      size: 120,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => `${fmtInt(r.quota.used)} / ${fmtInt(r.quota.limit)}`,
    },
    {
      id: 'r',
      header: '重置',
      size: 130,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) =>
        r.quota.nextResetAt ? (
          <span title={fmtDateTime(r.quota.nextResetAt.getTime())}>{fmtRelative(r.quota.nextResetAt.getTime())}</span>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
    {
      id: 'o',
      header: '超额',
      size: 110,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) =>
        r.quota.overageUsd > 0 ? fmtUsd(r.quota.overageUsd) : r.quota.overageEnabled ? <ToneBadge tone="warning">已开启</ToneBadge> : <span className="text-muted-foreground">—</span>,
    },
    {
      id: 's',
      header: '快照',
      size: 90,
      meta: { align: 'right' },
      cell: ({ row: { original: r } }) => <span className={r.quota.stale ? 'text-warning' : 'text-muted-foreground'}>{r.quota.checkedAt ? fmtRelative(r.quota.checkedAt.getTime()) : '未查询'}</span>,
    },
  ]
  const table = (data: AccountRow[], empty: string) =>
    page.isLoading ? (
      <LoadingRows />
    ) : (
      <DataTable
        className="rounded-none border-0"
        data={data}
        columns={columns}
        getRowId={(r) => String(r.id)}
        onRowClick={(r) => navigate({ to: '/accounts', search: { id: r.id } })}
        virtual={data.length > 30}
        maxHeight="24rem"
        empty={<EmptyState title={empty} className="py-6" />}
      />
    )

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <KpiCard
          label="启用账号剩余积分"
          value={c ? fmtCompact(c.enabledCreditRemaining) : '—'}
          hint={c ? `总额度 ${fmtCompact(c.enabledCreditLimit)} · 剩余 ${fmtPct(c.enabledCreditLimit ? c.enabledCreditRemaining / c.enabledCreditLimit : 0, 0)}` : undefined}
          loading={credit.isLoading}
        />
        <KpiCard label="即将耗尽（≥80%）" value={fmtInt(nearlyOut.length)} hint={`共 ${rows.length} 个启用账号`} loading={page.isLoading} />
        <KpiCard label="7 天内重置" value={fmtInt(resetting.length)} hint={resetting[0]?.quota.nextResetAt ? `最近：${fmtRelative(resetting[0].quota.nextResetAt.getTime())}` : undefined} loading={page.isLoading} />
        <KpiCard label="快照过期" value={fmtInt(stale.length)} hint={c?.lastCheckedAt ? `最近查询 ${fmtRelative(c.lastCheckedAt)}` : '超过 30 分钟未刷新'} loading={page.isLoading} />
      </div>

      <Section
        title="即将耗尽"
        description="使用率 ≥ 80% 或已超额"
        contentClassName="p-0"
        actions={
          stale.length > 0 && (
            <Button size="xs" variant="outline" onClick={() => actions.refreshInfo.mutate(stale.map((r) => r.id))} disabled={actions.refreshInfo.isPending}>
              <RefreshCw className={actions.refreshInfo.isPending ? 'animate-spin' : undefined} /> 刷新 {stale.length} 个过期快照
            </Button>
          )
        }
      >
        {table(nearlyOut, '没有即将耗尽的账号')}
      </Section>
      <div className="grid gap-4 xl:grid-cols-2">
        <Section title="即将重置" description="7 天内额度重置" contentClassName="p-0">
          {table(resetting, '7 天内没有账号重置')}
        </Section>
        <Section title="超额计费" description="已开启超额或已产生超额费用" contentClassName="p-0">
          {table(overage, '没有超额账号')}
        </Section>
      </div>
      {c && (
        <p className="text-xs text-muted-foreground">
          额度数据来自 Kiro getUsageLimits 快照；禁用账号剩余积分 {fmtCompact(c.disabledCreditRemaining)}（共 {c.disabledCredentials} 个）。
        </p>
      )}
    </div>
  )
}
