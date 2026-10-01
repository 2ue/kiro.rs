import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { Info } from 'lucide-react'
import { usageApi } from '@/api/endpoints/usage'
import { RankList, TrendChart } from '@/components/charts/trend-chart'
import { KpiCard } from '@/components/patterns/kpi-card'
import { EmptyState, LoadingRows } from '@/components/patterns/data-state'
import { Section } from '@/components/patterns/page-header'
import { fmtCompact, fmtInt, fmtPct, fmtUsd } from '@/lib/format'
import { qk } from '@/queries/keys'
import { useUsageSeries, useUsageTop, useUsageWindows } from '@/queries/shared'
import { usePollInterval, useUiPrefs } from '@/stores/ui-prefs'
import { USAGE_SOURCE_LABEL } from '@/domain/labels'
import type { UsageSource } from '@/api/types'

export function CostTab() {
  const windowKey = useUiPrefs((s) => s.windowKey)
  const windows = useUsageWindows()
  const series = useUsageSeries()
  const top = useUsageTop(windowKey)
  const navigate = useNavigate()
  const breakdown = useQuery({
    queryKey: qk.usage.breakdown(windowKey),
    queryFn: () => usageApi.breakdown(windowKey),
    refetchInterval: usePollInterval('normal'),
  })
  const w = windows.data?.windows.find((x) => x.key === windowKey)?.summary

  const daily = useMemo(
    () =>
      (series.data?.series.daily7d ?? []).map((p) => ({ label: p.label, cost: p.totalEstimatedCostUsd, original: p.totalOriginalCostUsd })),
    [series.data],
  )
  const tokens = useMemo(
    () => (series.data?.series.hourly24h ?? []).map((p) => ({ label: p.label, input: p.billableInputTokens, output: p.totalOutputTokens })),
    [series.data],
  )
  const cacheTotal = w ? w.totalInputTokens : 0

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2 rounded-lg border bg-muted/30 p-3 text-xs text-muted-foreground">
        <Info className="mt-0.5 size-3.5 shrink-0" />
        <p>
          估算费用按模型价格目录计算，不等于 Kiro 上游实际扣费；原价为未计入缓存折扣的价格。Kiro metering
          是上游返回的计量值。没有价格数据的请求不计入费用。
        </p>
      </div>
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
        <KpiCard
          label="估算费用"
          value={fmtUsd(w?.totalEstimatedCostUsd)}
          hint={w ? `原价 ${fmtUsd(w.totalOriginalCostUsd)}` : undefined}
          loading={windows.isLoading}
        />
        <KpiCard
          label="缓存节省"
          value={w ? fmtUsd(Math.max(0, w.totalOriginalCostUsd - w.totalEstimatedCostUsd)) : '—'}
          hint={w && w.totalOriginalCostUsd > 0 ? `节省 ${fmtPct(1 - w.totalEstimatedCostUsd / w.totalOriginalCostUsd, 0)}` : undefined}
          loading={windows.isLoading}
        />
        <KpiCard label="Kiro metering" value={fmtCompact(w?.totalKiroMeteringUsage)} loading={windows.isLoading} />
        <KpiCard
          label="计费输入 / 输出"
          value={w ? `${fmtCompact(w.billableInputTokens)} / ${fmtCompact(w.totalOutputTokens)}` : '—'}
          loading={windows.isLoading}
        />
        <KpiCard
          label="无价请求"
          value={fmtInt(w?.unpricedRequests)}
          hint={w ? `有价 ${fmtInt(w.pricedRequests)}` : undefined}
          loading={windows.isLoading}
        />
      </div>

      <div className="grid gap-4 xl:grid-cols-2">
        <Section title="近 7 天费用">
          {series.isLoading ? (
            <LoadingRows rows={5} />
          ) : (
            <TrendChart
              data={daily}
              xKey="label"
              kind="bar"
              stacked={false}
              valueFormatter={(v) => fmtUsd(v, v >= 100 ? 0 : 2)}
              series={[
                { key: 'cost', label: '估算费用', color: 'var(--chart-1)' },
                { key: 'original', label: '原价', color: 'var(--chart-8)' },
              ]}
            />
          )}
        </Section>
        <Section title="近 24 小时 Token">
          {series.isLoading ? (
            <LoadingRows rows={5} />
          ) : (
            <TrendChart
              data={tokens}
              xKey="label"
              series={[
                { key: 'input', label: '计费输入', color: 'var(--chart-2)' },
                { key: 'output', label: '输出', color: 'var(--chart-3)' },
              ]}
            />
          )}
        </Section>
      </div>

      <div className="grid gap-4 lg:grid-cols-3">
        <Section title="按模型费用">
          {top.isLoading ? (
            <LoadingRows />
          ) : (
            <RankList
              color="var(--chart-1)"
              valueFormatter={(v) => fmtUsd(v)}
              onSelect={(key) => navigate({ to: '/requests', search: { model: key } })}
              items={[...(top.data?.top.models ?? [])]
                .sort((a, b) => b.totalEstimatedCostUsd - a.totalEstimatedCostUsd)
                .slice(0, 10)
                .map((m) => ({ key: m.key, label: m.label ?? m.key, value: m.totalEstimatedCostUsd, sub: `${fmtInt(m.requests)} 次` }))}
            />
          )}
        </Section>
        <Section title="按账号费用">
          {top.isLoading ? (
            <LoadingRows />
          ) : (
            <RankList
              color="var(--chart-2)"
              valueFormatter={(v) => fmtUsd(v)}
              onSelect={(key) => Number.isFinite(Number(key)) && navigate({ to: '/accounts', search: { id: Number(key) } })}
              items={[...(top.data?.top.credentials ?? [])]
                .sort((a, b) => b.totalEstimatedCostUsd - a.totalEstimatedCostUsd)
                .slice(0, 10)
                .map((m) => ({
                  key: m.key,
                  label: m.label ?? `#${m.key}`,
                  value: m.totalEstimatedCostUsd,
                  sub: `${fmtInt(m.requests)} 次`,
                }))}
            />
          )}
        </Section>
        <Section title="缓存与用量来源">
          {!w ? (
            <LoadingRows />
          ) : (
            <div className="space-y-4">
              <RankList
                color="var(--chart-4)"
                items={[
                  {
                    key: 'read',
                    label: '缓存读',
                    value: w.totalCacheReadInputTokens,
                    sub: fmtPct(cacheTotal ? w.totalCacheReadInputTokens / cacheTotal : 0, 0),
                  },
                  {
                    key: 'write',
                    label: '缓存写',
                    value: w.totalCacheCreationInputTokens,
                    sub: fmtPct(cacheTotal ? w.totalCacheCreationInputTokens / cacheTotal : 0, 0),
                  },
                ]}
              />
              {(breakdown.data?.usageSourceBreakdown ?? []).length > 0 ? (
                <RankList
                  color="var(--chart-6)"
                  valueFormatter={(v) => fmtInt(v)}
                  items={(breakdown.data?.usageSourceBreakdown ?? []).map((b) => ({
                    key: b.key,
                    label: USAGE_SOURCE_LABEL[b.key as UsageSource] ?? b.label,
                    value: b.requests,
                    sub: fmtPct(b.ratio, 0),
                  }))}
                />
              ) : (
                <EmptyState title="无用量来源数据" className="py-4" />
              )}
              <p className="text-xs text-muted-foreground">
                会话粘性：{fmtInt(w.stickyBoundRequests)} 次绑定，{fmtInt(w.fallbackFromStickyRequests)} 次回退 · 缓存模拟{' '}
                {fmtInt(w.simulatedRequests)} 次
              </p>
            </div>
          )}
        </Section>
      </div>
    </div>
  )
}
