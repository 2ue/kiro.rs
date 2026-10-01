import { useMemo } from 'react'
import { Link, useNavigate } from '@tanstack/react-router'
import { ArrowRight, CircleCheck, Coins, Gauge, Timer, TriangleAlert, Zap } from 'lucide-react'
import { KpiCard } from '@/components/patterns/kpi-card'
import { Page, PageHeader, Section } from '@/components/patterns/page-header'
import { EmptyState, LoadingRows } from '@/components/patterns/data-state'
import { Meter, SegmentBar } from '@/components/status/meter'
import { StatusDot } from '@/components/status/tone-badge'
import { RankList, TrendChart } from '@/components/charts/trend-chart'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { fmtCompact, fmtInt, fmtMs, fmtPct, fmtUsd } from '@/lib/format'
import { usePersistedState } from '@/lib/use-persisted-state'
import { useCredentialSummary, usePoolsStatus, useUsageSeries, useUsageTop, useUsageWindows, useWriterStats } from '@/queries/shared'
import { useUiPrefs, WINDOW_OPTIONS } from '@/stores/ui-prefs'
import { buildAttention } from './attention'

/** 与当前窗口对比的上一周期 */
const PREV_WINDOW: Record<string, string | undefined> = { today: 'yesterday' }

export function OverviewPage() {
  const windowKey = useUiPrefs((s) => s.windowKey)
  const windowLabel = WINDOW_OPTIONS.find((w) => w.key === windowKey)?.label
  const summary = useCredentialSummary()
  const windows = useUsageWindows()
  const series = useUsageSeries()
  const top = useUsageTop(windowKey)
  const pools = usePoolsStatus()
  const writer = useWriterStats()
  const navigate = useNavigate()
  const [range, setRange] = usePersistedState<'24h' | '7d'>('overview.range', '24h')

  const current = windows.data?.windows.find((w) => w.key === windowKey)?.summary
  const prevKey = PREV_WINDOW[windowKey]
  const prev = prevKey ? windows.data?.windows.find((w) => w.key === prevKey)?.summary : undefined
  const delta = (a?: number, b?: number) => (a !== undefined && b !== undefined && b > 0 ? (a - b) / b : null)

  const points = range === '24h' ? series.data?.series.hourly24h : series.data?.series.daily7d
  const chartData = useMemo(
    () =>
      (points ?? []).map((p) => ({
        label: p.label,
        success: p.successRequests,
        error: p.errorRequests,
        cost: p.totalEstimatedCostUsd,
        input: p.totalInputTokens,
        output: p.totalOutputTokens,
      })),
    [points],
  )
  const spark = (key: 'success' | 'error' | 'cost' | 'input') => (series.data?.series.hourly24h ?? []).map((p) =>
    key === 'success' ? p.requests : key === 'error' ? p.errorRequests : key === 'cost' ? p.totalEstimatedCostUsd : p.totalInputTokens + p.totalOutputTokens,
  )

  const attention = buildAttention({ summary: summary.data, pools: pools.data, writer: writer.data, top: top.data?.top, errorRate: current?.errorRate })
  const s = summary.data
  const capacityMax = s?.globalMaxConcurrentRequests ?? 0
  const loading = windows.isLoading

  return (
    <Page>
      <PageHeader title="总览" description={`账号池容量、请求健康与需要处理的问题 · 统计窗口：${windowLabel}`} />

      <Section contentClassName="grid gap-6 lg:grid-cols-[1fr_1.4fr]">
        <div className="space-y-2">
          <div className="flex items-baseline justify-between">
            <span className="text-xs text-muted-foreground">全局容量水位</span>
            <span className="num text-sm">
              <span className="text-lg font-semibold">{fmtInt(s?.globalInFlightRequests)}</span>
              <span className="text-muted-foreground"> / {capacityMax > 0 ? fmtInt(capacityMax) : '不限'} 在途</span>
              {s && s.queuedRequests > 0 && <span className="ml-2 text-warning">排队 {s.queuedRequests}</span>}
            </span>
          </div>
          <Meter
            value={s?.globalInFlightRequests ?? 0}
            max={capacityMax > 0 ? capacityMax : Math.max(1, (s?.globalInFlightRequests ?? 0) * 2)}
            tone={capacityMax > 0 && (s?.globalInFlightRequests ?? 0) / capacityMax > 0.85 ? 'danger' : 'primary'}
            className="h-2"
            label="全局在途占用"
          />
          <p className="text-xs text-muted-foreground">
            {s ? `${fmtInt(s.schedulable)} 个账号可调度 · ${fmtInt(s.inUse)} 个正在处理请求` : '加载中…'}
          </p>
        </div>
        <div className="space-y-2">
          <span className="text-xs text-muted-foreground">账号池状态分布</span>
          {s ? (
            <SegmentBar
              onSelect={(key) => {
                const status = { cooling: 'cooldown', failing: 'error', disabled: 'disabled' }[key]
                navigate({ to: '/accounts', search: status ? { status: status as 'cooldown' } : {} })
              }}
              items={[
                { key: 'idle', label: '空闲可调度', value: Math.max(0, s.schedulable - s.inUse), tone: 'success' },
                { key: 'busy', label: '处理中', value: s.inUse, tone: 'info' },
                { key: 'cooling', label: '冷却/限流', value: s.coolingDown, tone: 'warning' },
                { key: 'failing', label: '异常', value: s.failing, tone: 'danger' },
                { key: 'disabled', label: '已禁用', value: s.disabled, tone: 'neutral' },
              ]}
            />
          ) : (
            <LoadingRows rows={1} />
          )}
        </div>
      </Section>

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
        <KpiCard
          label="请求数"
          icon={<CircleCheck className="size-3.5" />}
          loading={loading}
          value={fmtCompact(current?.totalRequests)}
          hint={current ? `成功 ${fmtCompact(current.successRequests)} · 流式 ${fmtPct(current.totalRequests ? current.streamRequests / current.totalRequests : 0, 0)}` : undefined}
          delta={delta(current?.totalRequests, prev?.totalRequests)}
          spark={spark('success')}
        />
        <KpiCard
          label="错误率"
          icon={<TriangleAlert className="size-3.5" />}
          loading={loading}
          value={fmtPct(current?.errorRate)}
          hint={current ? `${fmtInt(current.errorRequests)} 个失败请求` : undefined}
          delta={delta(current?.errorRate, prev?.errorRate)}
          deltaGoodWhenUp={false}
          spark={spark('error')}
          sparkColor="var(--danger)"
        />
        <KpiCard
          label="P95 耗时"
          icon={<Timer className="size-3.5" />}
          loading={loading}
          value={fmtMs(current?.p95DurationMs)}
          hint={current ? `平均 ${fmtMs(current.averageDurationMs)}` : undefined}
          delta={delta(current?.p95DurationMs, prev?.p95DurationMs)}
          deltaGoodWhenUp={false}
        />
        <KpiCard
          label="缓存命中"
          icon={<Zap className="size-3.5" />}
          loading={loading}
          value={fmtPct(current?.cacheReadRatio)}
          hint={current ? `缓存读 ${fmtCompact(current.totalCacheReadInputTokens)} tokens` : undefined}
          delta={delta(current?.cacheReadRatio, prev?.cacheReadRatio)}
        />
        <KpiCard
          label="估算费用"
          icon={<Coins className="size-3.5" />}
          loading={loading}
          value={fmtUsd(current?.totalEstimatedCostUsd)}
          hint={current ? `${fmtCompact(current.totalInputTokens + current.totalOutputTokens)} tokens · ${current.unpricedRequests} 个无价请求` : undefined}
          delta={delta(current?.totalEstimatedCostUsd, prev?.totalEstimatedCostUsd)}
          spark={spark('cost')}
          sparkColor="var(--chart-3)"
          className="col-span-2 lg:col-span-1"
        />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)]">
        <Section
          title="请求趋势"
          actions={
            <ToggleGroup type="single" size="sm" variant="outline" value={range} onValueChange={(v) => v && setRange(v as '24h' | '7d')}>
              <ToggleGroupItem value="24h">24 小时</ToggleGroupItem>
              <ToggleGroupItem value="7d">7 天</ToggleGroupItem>
            </ToggleGroup>
          }
        >
          {series.isLoading ? (
            <LoadingRows rows={5} />
          ) : chartData.length === 0 ? (
            <EmptyState title="暂无请求" />
          ) : (
            <TrendChart
              data={chartData}
              xKey="label"
              kind={range === '7d' ? 'bar' : 'area'}
              series={[
                { key: 'success', label: '成功', color: 'var(--chart-1)' },
                { key: 'error', label: '失败', color: 'var(--danger)' },
              ]}
            />
          )}
        </Section>

        <Section title="需要关注" description={attention.length ? `${attention.length} 项` : undefined} contentClassName="p-0">
          {attention.length === 0 ? (
            <EmptyState title="一切正常" description="没有需要处理的问题" icon={<CircleCheck className="text-success" />} />
          ) : (
            <ul className="divide-y">
              {attention.map((a) => (
                <li key={a.id}>
                  <Link
                    to={a.to}
                    search={a.search as never}
                    className="flex items-center gap-3 px-4 py-3 transition-colors hover:bg-muted/50"
                  >
                    <StatusDot tone={a.tone} pulse={a.tone === 'danger'} />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium">{a.title}</div>
                      {a.detail && <div className="truncate text-xs text-muted-foreground">{a.detail}</div>}
                    </div>
                    <ArrowRight className="size-4 text-muted-foreground" />
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </Section>
      </div>

      <div className="grid gap-4 lg:grid-cols-3">
        <TopSection
          title="热门模型"
          items={top.data?.top.models}
          total={top.data?.top.modelsTotal}
          truncated={top.data?.top.modelsTruncated}
          valueOf={(i) => i.requests}
          sub={(i) => fmtUsd(i.totalEstimatedCostUsd)}
          onSelect={(key) => navigate({ to: '/requests', search: { model: key } })}
          loading={top.isLoading}
        />
        <TopSection
          title="热门账号"
          items={top.data?.top.credentials}
          total={top.data?.top.credentialsTotal}
          truncated={top.data?.top.credentialsTruncated}
          valueOf={(i) => i.requests}
          sub={(i) => fmtUsd(i.totalEstimatedCostUsd)}
          onSelect={(key) => Number.isFinite(Number(key)) && navigate({ to: '/accounts', search: { id: Number(key) } })}
          loading={top.isLoading}
          color="var(--chart-2)"
        />
        <TopSection
          title="主要错误"
          items={top.data?.top.errors}
          total={top.data?.top.errorsTotal}
          truncated={top.data?.top.errorsTruncated}
          valueOf={(i) => i.requests}
          onSelect={(key) =>
            navigate({
              to: '/requests',
              search: ['error', 'stream_error', 'upstream_timeout', 'client_dropped'].includes(key) ? { status: key as 'error' } : { q: key },
            })
          }
          loading={top.isLoading}
          color="var(--danger)"
          emptyText="没有错误"
        />
      </div>
    </Page>
  )
}

function TopSection<T extends { key: string; label?: string }>({
  title,
  items,
  total,
  truncated,
  valueOf,
  sub,
  onSelect,
  loading,
  color,
  emptyText = '暂无数据',
}: {
  title: string
  items?: T[]
  total?: number
  truncated?: boolean
  valueOf: (i: T) => number
  sub?: (i: T) => string
  onSelect?: (key: string) => void
  loading?: boolean
  color?: string
  emptyText?: string
}) {
  return (
    <Section title={title} actions={<Gauge className="size-3.5 text-muted-foreground" />}>
      {loading ? (
        <LoadingRows rows={5} />
      ) : !items?.length ? (
        <EmptyState title={emptyText} className="py-6" />
      ) : (
        <RankList
          color={color}
          onSelect={onSelect}
          items={items.slice(0, 8).map((i) => ({ key: i.key, label: i.label ?? i.key, value: valueOf(i), sub: sub?.(i) }))}
          footer={
            truncated || (total && total > 8) ? (
              <p className="text-xs text-muted-foreground">仅显示前 {Math.min(8, items.length)} 项，共 {fmtInt(total)} 项</p>
            ) : undefined
          }
        />
      )}
    </Section>
  )
}
