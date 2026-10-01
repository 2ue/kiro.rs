import { useMemo } from 'react'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import type { ColumnDef } from '@tanstack/react-table'
import { Settings2 } from 'lucide-react'
import { credentialsApi } from '@/api/endpoints/credentials'
import { Button } from '@/components/ui/button'
import { DataTable } from '@/components/data-table/data-table'
import { RankList } from '@/components/charts/trend-chart'
import { KpiCard } from '@/components/patterns/kpi-card'
import { EmptyState, LoadingRows } from '@/components/patterns/data-state'
import { Page, PageHeader, Section } from '@/components/patterns/page-header'
import { CredentialStatusBadge, STATUS_META } from '@/components/status/credential-status-badge'
import { ToneBadge } from '@/components/status/tone-badge'
import type { PrimaryStatus } from '@/domain/credential-status'
import { LOAD_BALANCING_LABEL } from '@/domain/labels'
import { fmtDuration, fmtInt, fmtMs, fmtPct } from '@/lib/format'
import { qk } from '@/queries/keys'
import { useCredentialSummary, useLoadBalancing } from '@/queries/shared'
import { usePollInterval } from '@/stores/ui-prefs'
import { toAccountRow, type AccountRow } from '@/features/accounts/queries'

const BLOCKING: PrimaryStatus[] = ['disabled', 'rateLimited', 'cooling', 'saturated']

/**
 * 调度器视图：回答"调度为什么这样选"。
 * 数据来自启用账号的运行态快照，按调度评分（越低越优先）排序。
 */
export function SchedulerPage() {
  const lb = useLoadBalancing()
  const summary = useCredentialSummary()
  const navigate = useNavigate()
  const query = { page: 1, limit: 500, status: 'enabled', sortBy: 'scheduler_score' as const, sortOrder: 'asc' as const }
  const page = useQuery({
    queryKey: qk.credentials.page(query),
    queryFn: () => credentialsApi.page(query),
    refetchInterval: usePollInterval('fast'),
    placeholderData: keepPreviousData,
  })
  const rows = useMemo(() => (page.data?.credentials ?? []).map(toAccountRow), [page.data])

  const schedulable = rows.filter((r) => r.status.schedulable)
  const blocked = rows.filter((r) => !r.status.schedulable)
  const total5m = rows.reduce((sum, r) => sum + (r.recentSchedulerSelectionCount5m ?? 0), 0)
  const top = [...rows].sort((a, b) => (b.recentSchedulerSelectionCount5m ?? 0) - (a.recentSchedulerSelectionCount5m ?? 0)).slice(0, 15)
  const top3Share = total5m > 0 ? top.slice(0, 3).reduce((s, r) => s + (r.recentSchedulerSelectionCount5m ?? 0), 0) / total5m : 0

  const blockedGroups = BLOCKING.map((s) => ({ status: s, rows: blocked.filter((r) => r.status.primary === s) })).filter((g) => g.rows.length)

  const columns = useMemo<ColumnDef<AccountRow, unknown>[]>(
    () => [
      {
        id: 'rank',
        header: '#',
        size: 48,
        meta: { align: 'right' },
        cell: ({ row }) => <span className="text-muted-foreground">{row.index + 1}</span>,
      },
      {
        id: 'account',
        header: '账号',
        size: 220,
        meta: { grow: true },
        cell: ({ row: { original: r } }) => (
          <span className="truncate">
            {r.label} <span className="num text-xs text-muted-foreground">#{r.id}</span>
          </span>
        ),
      },
      { id: 'status', header: '状态', size: 130, cell: ({ row: { original: r } }) => <CredentialStatusBadge view={r.status} /> },
      {
        id: 'score',
        header: '评分',
        size: 90,
        meta: { align: 'right' },
        cell: ({ row: { original: r } }) => (r.schedulerScore ?? 0).toFixed(3),
      },
      {
        id: 'pressure',
        header: '选择压力',
        size: 90,
        meta: { align: 'right' },
        cell: ({ row: { original: r } }) => (r.schedulerSelectionPressure ?? 0).toFixed(2),
      },
      {
        id: 'sel',
        header: '选中 10s/60s/5m',
        size: 130,
        meta: { align: 'right' },
        cell: ({ row: { original: r } }) => (
          <span>
            {r.recentSchedulerSelectionCount10s ?? 0}
            <span className="text-muted-foreground"> / </span>
            {r.recentSchedulerSelectionCount60s ?? 0}
            <span className="text-muted-foreground"> / </span>
            {r.recentSchedulerSelectionCount5m ?? 0}
          </span>
        ),
      },
      {
        id: 'load',
        header: '在途',
        size: 72,
        meta: { align: 'right' },
        cell: ({ row: { original: r } }) => `${r.inFlightRequests}/${r.maxConcurrentRequests || '∞'}`,
      },
      { id: 'err', header: '错误率', size: 72, meta: { align: 'right' }, cell: ({ row: { original: r } }) => fmtPct(r.recentErrorRate ?? 0, 0) },
      { id: 'lat', header: '延迟', size: 72, meta: { align: 'right' }, cell: ({ row: { original: r } }) => fmtMs(r.latencyEwmaMs) },
      {
        id: 'prob',
        header: '观察期',
        size: 80,
        meta: { align: 'right' },
        cell: ({ row: { original: r } }) => (r.inProbation ? fmtDuration(r.probationRemainingSecs) : <span className="text-muted-foreground">—</span>),
      },
    ],
    [],
  )

  const mode = lb.data?.mode
  return (
    <Page>
      <PageHeader
        title="调度器"
        description="账号评分、选中分布与不可调度原因"
        actions={
          <Button variant="outline" size="sm" asChild>
            <Link to="/settings/$section" params={{ section: 'scheduling' }}>
              <Settings2 /> 调整调度参数
            </Link>
          </Button>
        }
      />

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <KpiCard
          label="负载均衡模式"
          value={mode ? LOAD_BALANCING_LABEL[mode].label : '—'}
          hint={
            <Link to="/settings/$section" params={{ section: 'balancing' }} className="hover:underline">
              {mode ? LOAD_BALANCING_LABEL[mode].desc : ''}
            </Link>
          }
        />
        <KpiCard label="可调度 / 启用" value={`${fmtInt(schedulable.length)} / ${fmtInt(rows.length)}`} hint={`${blocked.length} 个暂不可调度`} loading={page.isLoading} />
        <KpiCard label="5 分钟选中次数" value={fmtInt(total5m)} hint={summary.data ? `全局在途 ${summary.data.globalInFlightRequests}` : undefined} loading={page.isLoading} />
        <KpiCard
          label="流量集中度"
          value={fmtPct(top3Share, 0)}
          hint="前 3 个账号占 5 分钟选中比例"
          loading={page.isLoading}
          className={top3Share > 0.6 && rows.length > 6 ? 'border-warning/50' : undefined}
        />
      </div>

      {mode && mode !== 'health_balanced' && mode !== 'weighted_least_inflight' && (
        <p className="text-xs text-muted-foreground">
          当前为「{LOAD_BALANCING_LABEL[mode].label}」模式，调度评分仅作参考，实际按{mode === 'priority' ? '优先级' : '轮询均衡'}选择。
        </p>
      )}

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_22rem]">
        <Section title="候选排行" description="按调度评分升序（越靠前越优先）" contentClassName="p-0">
          {page.isLoading ? (
            <div className="p-4">
              <LoadingRows />
            </div>
          ) : (
            <DataTable
              className="rounded-none border-0"
              data={rows}
              columns={columns}
              getRowId={(r) => String(r.id)}
              onRowClick={(r) => navigate({ to: '/accounts', search: { id: r.id } })}
              virtual
              maxHeight="34rem"
              empty={<EmptyState title="没有启用的账号" />}
            />
          )}
        </Section>

        <div className="space-y-4">
          <Section title="5 分钟选中分布" description="Top 15">
            {total5m === 0 ? (
              <EmptyState title="近 5 分钟无调度" className="py-6" />
            ) : (
              <RankList
                onSelect={(key) => navigate({ to: '/accounts', search: { id: Number(key) } })}
                items={top
                  .filter((r) => (r.recentSchedulerSelectionCount5m ?? 0) > 0)
                  .map((r) => ({
                    key: String(r.id),
                    label: r.label,
                    value: r.recentSchedulerSelectionCount5m ?? 0,
                    sub: fmtPct((r.recentSchedulerSelectionCount5m ?? 0) / total5m, 0),
                  }))}
                valueFormatter={(v) => fmtInt(v)}
              />
            )}
          </Section>

          <Section title="不可调度" description={blocked.length ? `${blocked.length} 个启用账号` : undefined} contentClassName="p-0">
            {blockedGroups.length === 0 ? (
              <EmptyState title="所有启用账号均可调度" className="py-6" />
            ) : (
              <div className="divide-y">
                {blockedGroups.map((g) => (
                  <div key={g.status} className="p-3">
                    <div className="mb-2 flex items-center gap-2">
                      <ToneBadge tone={STATUS_META[g.status].tone}>{STATUS_META[g.status].label}</ToneBadge>
                      <span className="num text-xs text-muted-foreground">{g.rows.length} 个</span>
                    </div>
                    <ul className="space-y-1">
                      {g.rows.slice(0, 8).map((r) => (
                        <li key={r.id}>
                          <Link to="/accounts" search={{ id: r.id }} className="flex items-center justify-between gap-2 rounded px-1 text-xs hover:bg-muted">
                            <span className="truncate">{r.label}</span>
                            <span className="num shrink-0 text-muted-foreground">
                              {r.status.remainingSecs ? `${fmtDuration(r.status.remainingSecs)} 后恢复` : r.status.detail}
                            </span>
                          </Link>
                        </li>
                      ))}
                      {g.rows.length > 8 && <li className="px-1 text-xs text-muted-foreground">还有 {g.rows.length - 8} 个…</li>}
                    </ul>
                  </div>
                ))}
              </div>
            )}
          </Section>
        </div>
      </div>
      {page.data && page.data.filteredTotal > rows.length && (
        <p className="text-xs text-muted-foreground">启用账号共 {fmtInt(page.data.filteredTotal)} 个，这里只分析评分最优的前 {rows.length} 个。</p>
      )}
    </Page>
  )
}
