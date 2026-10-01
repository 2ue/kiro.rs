import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import type { ColumnDef } from '@tanstack/react-table'
import { usageApi } from '@/api/endpoints/usage'
import type { UsageExternalPoolBillingByPool, UsageExternalPoolRiskGroup, UsageExternalPoolRiskSample } from '@/api/types'
import { DataTable } from '@/components/data-table/data-table'
import { KpiCard } from '@/components/patterns/kpi-card'
import { EmptyState, ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { Section } from '@/components/patterns/page-header'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtCompact, fmtDateTime, fmtInt, fmtPct, fmtUsd } from '@/lib/format'
import { qk } from '@/queries/keys'
import { usePollInterval, useUiPrefs } from '@/stores/ui-prefs'

const signed = (v: number | undefined) => (
  <span className={(v ?? 0) < 0 ? 'text-danger' : (v ?? 0) > 0 ? 'text-success' : ''}>{fmtUsd(v)}</span>
)

/** 外部池盈亏与风控：合并了原"外部池计费"和"外部池风控"两处 */
export function PoolsBillingTab() {
  const windowKey = useUiPrefs((s) => s.windowKey)
  const interval = usePollInterval('normal')
  const navigate = useNavigate()
  const billing = useQuery({
    queryKey: qk.usage.poolBilling(windowKey),
    queryFn: () => usageApi.poolBilling(windowKey),
    refetchInterval: interval,
  })
  const risk = useQuery({
    queryKey: qk.usage.poolRisk({ windowKey }),
    queryFn: () => usageApi.poolRisk({ windowKey, limit: 50 }),
    refetchInterval: interval,
  })

  const pools = billing.data?.externalPoolBillingByPool ?? []
  const totals = useMemo(
    () =>
      pools.reduce(
        (a, p) => ({
          requests: a.requests + p.requests,
          raw: a.raw + p.rawCostUsd,
          billable: a.billable + p.billableCostUsd,
          profit: a.profit + (p.profitUsd ?? 0),
          floor: a.floor + p.costFloorAppliedRequests,
        }),
        { requests: 0, raw: 0, billable: 0, profit: 0, floor: 0 },
      ),
    [pools],
  )

  const poolColumns: ColumnDef<UsageExternalPoolBillingByPool, unknown>[] = [
    {
      id: 'name',
      header: '外部池',
      size: 180,
      meta: { mobile: 'title', grow: true },
      cell: ({ row: { original: p } }) => (
        <Link to="/pools" search={{ id: p.poolId }} className="hover:underline" onClick={(e) => e.stopPropagation()}>
          {p.poolName}
        </Link>
      ),
    },
    { id: 'req', header: '请求', size: 80, meta: { mobile: true, align: 'right' }, cell: ({ row: { original: p } }) => fmtInt(p.requests) },
    {
      id: 'raw',
      header: '上游成本',
      size: 100,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: p } }) => fmtUsd(p.rawCostUsd),
    },
    { id: 'shaped', header: '整形后', size: 100, meta: { align: 'right' }, cell: ({ row: { original: p } }) => fmtUsd(p.shapedCostUsd) },
    { id: 'reported', header: '上报', size: 100, meta: { align: 'right' }, cell: ({ row: { original: p } }) => fmtUsd(p.reportedCostUsd) },
    {
      id: 'billable',
      header: '计费',
      size: 100,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: p } }) => fmtUsd(p.billableCostUsd),
    },
    {
      id: 'profit',
      header: '利润',
      size: 100,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: p } }) => signed(p.profitUsd),
    },
    {
      id: 'margin',
      header: '毛利率',
      size: 80,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: p } }) => (p.billableCostUsd > 0 ? fmtPct((p.profitUsd ?? 0) / p.billableCostUsd, 0) : '—'),
    },
    {
      id: 'floor',
      header: '成本补齐',
      size: 110,
      meta: { align: 'right' },
      cell: ({ row: { original: p } }) =>
        p.costFloorAppliedRequests ? `${p.costFloorAppliedRequests} 次 · ${fmtUsd(p.costFloorDeltaUsd)}` : '—',
    },
    {
      id: 'unpriced',
      header: '无价',
      size: 64,
      meta: { align: 'right' },
      cell: ({ row: { original: p } }) => (p.unpricedRequests ? <span className="text-warning">{p.unpricedRequests}</span> : '0'),
    },
  ]

  const groupColumns: ColumnDef<UsageExternalPoolRiskGroup, unknown>[] = [
    {
      id: 'label',
      header: '维度',
      size: 200,
      meta: { mobile: 'title', grow: true },
      cell: ({ row: { original: g } }) => <span className="truncate font-mono text-xs">{g.label}</span>,
    },
    {
      id: 'records',
      header: '请求',
      size: 72,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: g } }) => fmtInt(g.records),
    },
    {
      id: 'warn',
      header: '缓存告警/严重',
      size: 110,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: g } }) => (
        <span>
          <span className={g.warningRecords ? 'text-warning' : ''}>{g.warningRecords}</span> /{' '}
          <span className={g.criticalRecords ? 'text-danger' : ''}>{g.criticalRecords}</span>
        </span>
      ),
    },
    {
      id: 'below',
      header: '低于成本',
      size: 80,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: g } }) => (g.belowRawCount ? <span className="text-danger">{g.belowRawCount}</span> : '0'),
    },
    {
      id: 'belowT',
      header: '低于目标',
      size: 80,
      meta: { align: 'right' },
      cell: ({ row: { original: g } }) => (g.belowTargetCount ? <span className="text-warning">{g.belowTargetCount}</span> : '0'),
    },
    {
      id: 'loss',
      header: '亏损',
      size: 90,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: g } }) => (g.totalLossUsd ? <span className="text-danger">{fmtUsd(g.totalLossUsd)}</span> : '—'),
    },
    {
      id: 'profit',
      header: '利润',
      size: 90,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: g } }) => signed(g.profitUsd),
    },
  ]

  const sampleColumns: ColumnDef<UsageExternalPoolRiskSample, unknown>[] = [
    {
      id: 't',
      header: '时间',
      size: 120,
      meta: { mobile: true },
      cell: ({ row: { original: s } }) => <span className="num text-muted-foreground">{fmtDateTime(s.createdAt, true)}</span>,
    },
    { id: 'pool', header: '外部池', size: 120, meta: { mobile: 'title' }, cell: ({ row: { original: s } }) => s.externalPoolName ?? '—' },
    {
      id: 'model',
      header: '模型',
      size: 160,
      meta: { mobile: true, grow: true },
      cell: ({ row: { original: s } }) => <span className="truncate font-mono text-xs">{s.model}</span>,
    },
    {
      id: 'cache',
      header: '缓存读 原始→上报',
      size: 140,
      meta: { align: 'right' },
      cell: ({ row: { original: s } }) => `${fmtCompact(s.rawCacheReadInputTokens)} → ${fmtCompact(s.reportedCacheReadInputTokens)}`,
    },
    {
      id: 'cost',
      header: '成本 原始→上报',
      size: 140,
      meta: { mobile: true, align: 'right' },
      cell: ({ row: { original: s } }) => `${fmtUsd(s.rawCostUsd)} → ${fmtUsd(s.reportedCostUsd)}`,
    },
    {
      id: 'why',
      header: '风险',
      size: 180,
      meta: { mobile: true },
      cell: ({ row: { original: s } }) => (
        <span className="flex flex-wrap gap-1">
          {s.riskReasons.slice(0, 2).map((r) => (
            <ToneBadge key={r} tone="warning">
              {r}
            </ToneBadge>
          ))}
        </span>
      ),
    },
  ]

  const r = risk.data
  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <KpiCard label="外部池请求" value={fmtInt(totals.requests)} loading={billing.isLoading} />
        <KpiCard label="上游原始成本" value={fmtUsd(totals.raw)} loading={billing.isLoading} />
        <KpiCard
          label="计费收入"
          value={fmtUsd(totals.billable)}
          hint={totals.floor ? `${totals.floor} 次触发成本底线补齐` : undefined}
          loading={billing.isLoading}
        />
        <KpiCard
          label="利润"
          value={signed(totals.profit)}
          hint={totals.billable > 0 ? `毛利率 ${fmtPct(totals.profit / totals.billable, 0)}` : undefined}
          loading={billing.isLoading}
        />
      </div>
      <Section title="按外部池" description="计费 = 对外上报口径经成本底线处理后的金额" contentClassName="p-0">
        {billing.isLoading ? (
          <div className="p-4">
            <LoadingRows />
          </div>
        ) : billing.error ? (
          <ErrorState error={billing.error} />
        ) : (
          <DataTable
            className="rounded-none border-0"
            data={pools}
            columns={poolColumns}
            getRowId={(p) => String(p.poolId)}
            empty={<EmptyState title="该窗口没有外部池请求" className="py-6" />}
          />
        )}
      </Section>

      <Section
        title="风控"
        description={
          r
            ? `阈值：缓存 ≥ ${fmtCompact(r.thresholds.warningTokens)} 告警，≥ ${fmtCompact(r.thresholds.criticalTokens)} 严重 · 目标成本倍数 ${r.thresholds.costTargetMultiplier}`
            : undefined
        }
        contentClassName="space-y-4"
      >
        {risk.isLoading ? (
          <LoadingRows />
        ) : risk.error ? (
          <ErrorState error={risk.error} />
        ) : r ? (
          <>
            <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
              <KpiCard
                label="分析请求"
                value={fmtInt(r.totals.records)}
                hint={`${r.totals.missingExternalPoolBillingRecords} 条缺少计费数据`}
              />
              <KpiCard
                label="缓存严重"
                value={fmtInt(r.reportedCache.eitherCriticalCount)}
                hint={`告警 ${r.reportedCache.eitherWarningCount}`}
              />
              <KpiCard label="低于上游成本" value={fmtInt(r.cost.belowRawCount)} hint={`累计亏损 ${fmtUsd(r.cost.totalLossUsd)}`} />
              <KpiCard label="低于目标成本" value={fmtInt(r.cost.belowTargetCount)} hint={`目标差额 ${fmtUsd(r.cost.totalTargetGapUsd)}`} />
              <KpiCard label="输出为 0" value={fmtInt(r.totals.outputZeroRecords)} hint="可能是空响应或错误" />
            </div>
            <div className="grid gap-4 xl:grid-cols-2">
              <div className="overflow-hidden rounded-lg border">
                <div className="border-b bg-muted/40 px-3 py-2 text-xs font-medium">按模型</div>
                <DataTable
                  className="rounded-none border-0"
                  data={r.byModel}
                  columns={groupColumns}
                  getRowId={(g) => g.key}
                  empty={<EmptyState title="无数据" className="py-4" />}
                />
              </div>
              <div className="overflow-hidden rounded-lg border">
                <div className="border-b bg-muted/40 px-3 py-2 text-xs font-medium">按入口路径</div>
                <DataTable
                  className="rounded-none border-0"
                  data={r.byPath}
                  columns={groupColumns}
                  getRowId={(g) => g.key}
                  empty={<EmptyState title="无数据" className="py-4" />}
                />
              </div>
            </div>
            <div className="overflow-hidden rounded-lg border">
              <div className="border-b bg-muted/40 px-3 py-2 text-xs font-medium">风险样本（点击查看请求详情）</div>
              <DataTable
                className="rounded-none border-0"
                data={r.samples}
                columns={sampleColumns}
                getRowId={(s) => s.id}
                onRowClick={(s) => navigate({ to: '/requests', search: { id: s.id } })}
                empty={<EmptyState title="没有风险样本" className="py-4" />}
              />
            </div>
          </>
        ) : null}
      </Section>
    </div>
  )
}
