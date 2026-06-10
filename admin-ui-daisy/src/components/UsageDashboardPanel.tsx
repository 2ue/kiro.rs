import { useMemo, useState } from 'react'
import type { ReactNode } from 'react'
import {
  Activity,
  AlertTriangle,
  BarChart3,
  CheckCircle2,
  Clock3,
  Database,
  DollarSign,
  Gauge,
  LineChart,
  ShieldAlert,
  Zap,
} from 'lucide-react'
import { Button } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, LoadingState } from '@/components/ui'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { useUsageDashboard } from '@/hooks/use-usage'
import { formatDate, formatNumber, formatPercent, formatUsd } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type {
  UsageBreakdownItem,
  UsageDashboardWindow,
  UsageExternalPoolBillingByPool,
  UsageExternalPoolBillingSummary,
  UsageSeriesPoint,
  UsageTopAggregate,
} from '@/types/api'

const DASHBOARD_TIMEZONE = 'Asia/Shanghai'
const DASHBOARD_AUTO_REFRESH_KEY = 'kiro-admin:auto-refresh:dashboard'

type DashboardTone = 'default' | 'success' | 'warning' | 'error' | 'info' | 'primary'
type RankDimension = 'models' | 'credentials' | 'endpoints' | 'errors'
type BillingDeltaTone = 'loss' | 'profit' | 'even'

const EMPTY_EXTERNAL_POOL_BILLING: UsageExternalPoolBillingSummary = {
  requests: 0,
  pricedRequests: 0,
  unpricedRequests: 0,
  costFloorAppliedRequests: 0,
  rawCostUsd: 0,
  shapedCostUsd: 0,
  upliftedCostUsd: 0,
  profitUsd: 0,
  reportedCostUsd: 0,
  billableCostUsd: 0,
  costFloorDeltaUsd: 0,
}

const EMPTY_TOP = {
  models: [] as UsageTopAggregate[],
  credentials: [] as UsageTopAggregate[],
  endpoints: [] as UsageTopAggregate[],
  errors: [] as UsageTopAggregate[],
}

const rankDimensions: Array<{ key: RankDimension; label: string }> = [
  { key: 'models', label: '模型' },
  { key: 'credentials', label: '账号' },
  { key: 'endpoints', label: 'Endpoint' },
  { key: 'errors', label: '错误' },
]

const toneClass: Record<DashboardTone, { text: string; soft: string; border: string; bar: string }> = {
  default: {
    text: 'text-base-content',
    soft: 'bg-base-200/45',
    border: 'border-base-300/60',
    bar: 'bg-base-content/35',
  },
  success: {
    text: 'text-success',
    soft: 'bg-base-100',
    border: 'border-success/20',
    bar: 'bg-success/80',
  },
  warning: {
    text: 'text-warning',
    soft: 'bg-base-100',
    border: 'border-warning/25',
    bar: 'bg-warning/80',
  },
  error: {
    text: 'text-error',
    soft: 'bg-base-100',
    border: 'border-error/25',
    bar: 'bg-error/80',
  },
  info: {
    text: 'text-info',
    soft: 'bg-base-100',
    border: 'border-info/20',
    bar: 'bg-info/70',
  },
  primary: {
    text: 'text-primary',
    soft: 'bg-base-100',
    border: 'border-primary/20',
    bar: 'bg-primary/70',
  },
}

function activeWindow(windows: UsageDashboardWindow[], key: string): UsageDashboardWindow | undefined {
  return windows.find((window) => window.key === key) || windows[0]
}

function billingDeltaTone(delta: number): BillingDeltaTone {
  if (delta < 0) return 'loss'
  if (delta > 0) return 'profit'
  return 'even'
}

function billingDeltaTextClass(tone: BillingDeltaTone): string {
  if (tone === 'loss') return 'text-error'
  if (tone === 'profit') return 'text-warning'
  return 'text-base-content/50'
}

function Panel({
  title,
  subtitle,
  actions,
  children,
  className = '',
}: {
  title: string
  subtitle?: ReactNode
  actions?: ReactNode
  children: ReactNode
  className?: string
}) {
  return (
    <section className={`section-card overflow-hidden rounded-box ${className}`}>
      <div className="flex flex-col gap-1.5 border-b border-base-300/60 px-3 py-2.5 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <h2 className="truncate text-sm font-semibold tracking-tight">{title}</h2>
          {subtitle && <div className="text-[0.68rem] leading-4 text-base-content/50">{subtitle}</div>}
        </div>
        {actions && <div className="flex shrink-0 flex-wrap items-center gap-1.5">{actions}</div>}
      </div>
      {children}
    </section>
  )
}

function MetricTile({
  title,
  value,
  desc,
  icon,
  tone = 'default',
}: {
  title: string
  value: string
  desc?: string
  icon: ReactNode
  tone?: DashboardTone
}) {
  const styles = toneClass[tone]

  return (
    <div className="relative overflow-hidden rounded-box border border-base-300/60 bg-base-100 p-3 shadow-sm">
      <div className={`absolute inset-x-0 top-0 h-0.5 ${styles.bar}`} />
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="text-[0.64rem] font-semibold uppercase text-base-content/45">{title}</div>
          <div className={`mt-1 truncate text-xl font-bold leading-6 ${styles.text}`}>{value}</div>
          {desc && <div className="mt-0.5 truncate text-[0.66rem] text-base-content/50">{desc}</div>}
        </div>
        <div className={`rounded-md border border-base-300/60 bg-base-100 p-1.5 ${styles.text}`}>{icon}</div>
      </div>
    </div>
  )
}

function DashboardToolbar({
  data,
  selectedWindow,
  onWindowChange,
  autoRefreshEnabled,
  autoRefreshSeconds,
  onAutoRefreshEnabledChange,
  onAutoRefreshSecondsChange,
}: {
  data: NonNullable<ReturnType<typeof useUsageDashboard>['data']>
  selectedWindow: UsageDashboardWindow
  onWindowChange: (key: string) => void
  autoRefreshEnabled: boolean
  autoRefreshSeconds: number
  onAutoRefreshEnabledChange: (enabled: boolean) => void
  onAutoRefreshSecondsChange: (seconds: number) => void
}) {
  return (
    <div className="rounded-box border border-base-300/60 bg-base-100 px-4 py-3 shadow-sm">
      <div className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold">{selectedWindow.label}</span>
            <Badge tone="neutral">{data.timezone}</Badge>
            <Badge tone={autoRefreshEnabled ? 'info' : 'neutral'}>
              {autoRefreshEnabled ? `自动刷新 ${autoRefreshSeconds}s` : '自动刷新关闭'}
            </Badge>
          </div>
          <div className="mt-1 text-xs text-base-content/50">
            {formatDate(selectedWindow.from)} - {formatDate(selectedWindow.to)} · 生成 {formatDate(data.generatedAt)}
          </div>
        </div>

        <div className="flex flex-col gap-2 xl:items-end">
          <div className="join overflow-x-auto rounded-box">
            {data.windows.map((window) => (
              <Button
                key={window.key}
                type="button"
                className="join-item shrink-0"
                size="sm"
                color={window.key === selectedWindow.key ? 'primary' : 'ghost'}
                variant={window.key === selectedWindow.key ? undefined : 'outline'}
                onClick={() => onWindowChange(window.key)}
              >
                {window.label}
              </Button>
            ))}
          </div>
          <div className="flex flex-wrap items-center gap-2 text-xs text-base-content/60">
            <label className="flex cursor-pointer items-center gap-2">
              <input
                type="checkbox"
                className="toggle toggle-primary toggle-sm"
                checked={autoRefreshEnabled}
                onChange={(event) => onAutoRefreshEnabledChange(event.target.checked)}
              />
              自动刷新
            </label>
            <input
              type="number"
              min={5}
              max={3600}
              className="input input-bordered input-xs w-20"
              value={autoRefreshSeconds}
              disabled={!autoRefreshEnabled}
              onChange={(event) => onAutoRefreshSecondsChange(Number(event.target.value))}
            />
            <span>秒</span>
          </div>
        </div>
      </div>
    </div>
  )
}

function SeriesChart({ title, points }: { title: string; points: UsageSeriesPoint[] }) {
  const maxRequests = Math.max(...points.map((point) => point.requests), 1)
  const totalRequests = points.reduce((sum, point) => sum + point.requests, 0)
  const totalCost = points.reduce((sum, point) => sum + point.totalEstimatedCostUsd, 0)
  const totalErrors = points.reduce((sum, point) => sum + point.errorRequests, 0)

  return (
    <Panel
      title={title}
      subtitle={`${formatNumber(totalRequests)} 请求 · ${formatUsd(totalCost)}`}
      actions={totalErrors > 0 ? <Badge tone="error">错误 {formatNumber(totalErrors)}</Badge> : <Badge tone="success">无错误</Badge>}
    >
      <div className="overflow-x-auto px-3 py-3">
        {points.length === 0 ? (
          <div className="flex h-32 items-center justify-center text-sm text-base-content/45">暂无数据</div>
        ) : (
          <div className="flex h-32 min-w-[520px] items-end gap-1.5">
            {points.map((point) => {
              const height = Math.max(6, Math.round((point.requests / maxRequests) * 96))
              const errorHeight = point.requests > 0
                ? Math.max(point.errorRequests > 0 ? 4 : 0, Math.round((point.errorRequests / point.requests) * height))
                : 0
              return (
                <div key={point.key} className="group flex min-w-0 flex-1 flex-col items-center gap-1">
                  <div
                    className="relative w-full overflow-hidden rounded-t bg-primary/45 transition-colors group-hover:bg-primary/65"
                    style={{ height }}
                    title={`${point.label}: ${formatNumber(point.requests)} 请求 / ${formatNumber(point.errorRequests)} 错误 / ${formatUsd(point.totalEstimatedCostUsd)}`}
                  >
                    {errorHeight > 0 && <div className="absolute inset-x-0 bottom-0 bg-error/70" style={{ height: errorHeight }} />}
                  </div>
                  <span className="w-full truncate text-center text-[0.65rem] text-base-content/45">{point.label}</span>
                </div>
              )
            })}
          </div>
        )}
      </div>
    </Panel>
  )
}

function SignalRow({
  label,
  value,
  ratio,
  tone = 'primary',
}: {
  label: string
  value: ReactNode
  ratio?: number
  tone?: DashboardTone
}) {
  const styles = toneClass[tone]
  const width = Number.isFinite(ratio ?? Number.NaN) ? Math.min(100, Math.max(0, (ratio as number) * 100)) : 0

  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="truncate font-medium text-base-content/75">{label}</span>
        <span className="shrink-0 font-mono text-xs text-base-content/55">{value}</span>
      </div>
      {ratio !== undefined && (
        <div className="h-1.5 overflow-hidden rounded-full bg-base-300/60">
          <div className={`h-full rounded-full ${styles.bar}`} style={{ width: `${width}%` }} />
        </div>
      )}
    </div>
  )
}

function BreakdownPanel({
  title,
  subtitle,
  items,
  emptyText,
}: {
  title: string
  subtitle?: string
  items: UsageBreakdownItem[]
  emptyText: string
}) {
  return (
    <Panel title={title} subtitle={subtitle}>
      <div className="space-y-3 p-3">
        {items.length === 0 ? (
          <div className="rounded-box border border-dashed border-base-300 p-3 text-sm text-base-content/45">{emptyText}</div>
        ) : (
          items.slice(0, 6).map((item) => (
            <SignalRow
              key={item.key}
              label={item.label}
              value={`${formatNumber(item.requests)} · ${formatPercent(item.ratio)}`}
              ratio={item.ratio}
              tone={item.key === 'success' ? 'success' : item.key.includes('timeout') || item.key.includes('error') ? 'error' : 'primary'}
            />
          ))
        )}
      </div>
    </Panel>
  )
}

function ErrorFocusPanel({
  totalErrors,
  items,
}: {
  totalErrors: number
  items: UsageTopAggregate[]
}) {
  const visibleItems = items.filter((item) => item.requests > 0).slice(0, 4)

  return (
    <Panel
      title="异常摘要"
      subtitle="只展示需要排障的错误聚合，完整明细到用量记录页筛选"
      actions={totalErrors > 0 ? <Badge tone="error">{formatNumber(totalErrors)} 错误</Badge> : <Badge tone="success">正常</Badge>}
    >
      <div className="space-y-3 p-3">
        {visibleItems.length === 0 ? (
          <div className="flex items-center gap-2 rounded-box border border-base-300/60 bg-base-100 p-3 text-sm text-base-content/65">
            <CheckCircle2 className="h-4 w-4 shrink-0" />
            当前窗口没有错误聚合。
          </div>
        ) : (
          visibleItems.map((item, index) => (
            <div key={`${item.key}-${index}`} className="relative overflow-hidden rounded-box border border-base-300/60 bg-base-100 p-3">
              <div className="absolute inset-y-3 left-0 w-0.5 rounded-r bg-error/80" />
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-xs font-semibold text-error" title={item.label || item.key}>
                    {item.label || item.key}
                  </div>
                  {item.label && <div className="truncate font-mono text-[0.62rem] text-base-content/45">{item.key}</div>}
                </div>
                <Badge tone="error">{formatNumber(item.requests)}</Badge>
              </div>
              <div className="mt-2 grid grid-cols-3 gap-1.5 text-[0.62rem] text-base-content/50">
                <span className="truncate">输入 {formatNumber(item.totalInputTokens)}</span>
                <span className="truncate">输出 {formatNumber(item.totalOutputTokens)}</span>
                <span className="truncate text-right">{formatUsd(item.totalEstimatedCostUsd)}</span>
              </div>
            </div>
          ))
        )}
      </div>
    </Panel>
  )
}

function DimensionRankPanel({
  top,
  activeKey,
  onActiveKeyChange,
}: {
  top: {
    models: UsageTopAggregate[]
    credentials: UsageTopAggregate[]
    endpoints: UsageTopAggregate[]
    errors: UsageTopAggregate[]
  }
  activeKey: RankDimension
  onActiveKeyChange: (key: RankDimension) => void
}) {
  const items = top[activeKey] || []
  const totalRequests = items.reduce((sum, item) => sum + item.requests, 0)

  return (
    <Panel
      title="维度排行"
      subtitle="保留后端 Top 聚合，切换维度查看，不占用总览主视图空间"
      actions={
        <div className="join rounded-box">
          {rankDimensions.map((dimension) => (
            <Button
              key={dimension.key}
              type="button"
              className="join-item"
              size="xs"
              color={dimension.key === activeKey ? 'primary' : 'ghost'}
              variant={dimension.key === activeKey ? undefined : 'outline'}
              onClick={() => onActiveKeyChange(dimension.key)}
            >
              {dimension.label}
            </Button>
          ))}
        </div>
      }
    >
      <div className="divide-y divide-base-300/60">
        {items.length === 0 ? (
          <div className="p-3 text-sm text-base-content/45">暂无排行数据。</div>
        ) : (
          items.map((item, index) => (
            <div key={`${activeKey}-${item.key}-${index}`} className="grid gap-2 px-3 py-2.5 md:grid-cols-[minmax(0,1fr)_220px] md:items-center">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded bg-base-200 font-mono text-[0.62rem] font-semibold text-base-content/45">
                    {index + 1}
                  </span>
                  <span className="truncate text-xs font-semibold" title={item.label || item.key}>
                    {item.label || item.key}
                  </span>
                  {item.errorRequests > 0 && <Badge tone="error" size="xs">错 {formatNumber(item.errorRequests)}</Badge>}
                </div>
                {item.label && <div className="mt-0.5 truncate pl-7 font-mono text-[0.62rem] text-base-content/45">{item.key}</div>}
                <div className="mt-1.5 pl-7">
                  <div className="h-1.5 overflow-hidden rounded-full bg-base-300/60">
                    <div className="h-full rounded-full bg-primary/65" style={{ width: `${totalRequests > 0 ? Math.min(100, (item.requests / totalRequests) * 100) : 0}%` }} />
                  </div>
                </div>
              </div>
              <div className="grid grid-cols-4 gap-1.5 text-right text-[0.62rem] text-base-content/50">
                <span className="truncate">请求 {formatNumber(item.requests)}</span>
                <span className="truncate">输入 {formatNumber(item.totalInputTokens)}</span>
                <span className="truncate">输出 {formatNumber(item.totalOutputTokens)}</span>
                <span className="truncate">{formatUsd(item.totalEstimatedCostUsd)}</span>
              </div>
            </div>
          ))
        )}
      </div>
    </Panel>
  )
}

function OperationsPanel({
  pricedRatio,
  streamRatio,
  cacheReadRatio,
  stickyBoundRequests,
  fallbackFromStickyRequests,
  simulatedRequests,
  upstreamMetadataRequests,
}: {
  pricedRatio: number
  streamRatio: number
  cacheReadRatio: number
  stickyBoundRequests: number
  fallbackFromStickyRequests: number
  simulatedRequests: number
  upstreamMetadataRequests: number
}) {
  return (
    <Panel title="运行信号" subtitle="这些指标更适合总览页判断配置和调度是否异常">
      <div className="grid gap-3 p-3 md:grid-cols-2">
        <SignalRow label="计价覆盖" value={formatPercent(pricedRatio)} ratio={pricedRatio} tone={pricedRatio < 1 ? 'warning' : 'success'} />
        <SignalRow label="流式占比" value={formatPercent(streamRatio)} ratio={streamRatio} tone="info" />
        <SignalRow label="缓存读取率" value={formatPercent(cacheReadRatio)} ratio={cacheReadRatio} tone="success" />
        <SignalRow
          label="Sticky 回退"
          value={`${formatNumber(fallbackFromStickyRequests)} / sticky ${formatNumber(stickyBoundRequests)}`}
          ratio={stickyBoundRequests > 0 ? fallbackFromStickyRequests / stickyBoundRequests : 0}
          tone={fallbackFromStickyRequests > 0 ? 'warning' : 'default'}
        />
        <SignalRow label="模拟用量" value={formatNumber(simulatedRequests)} tone={simulatedRequests > 0 ? 'info' : 'default'} />
        <SignalRow label="上游元数据" value={formatNumber(upstreamMetadataRequests)} tone="primary" />
      </div>
    </Panel>
  )
}

function ExternalPoolBillingPanel({
  billing,
  billingByPool,
}: {
  billing: UsageExternalPoolBillingSummary
  billingByPool: UsageExternalPoolBillingByPool[]
}) {
  const shapedCost = billing.shapedCostUsd ?? billing.reportedCostUsd ?? 0
  const upliftedCost = billing.upliftedCostUsd ?? billing.reportedCostUsd ?? billing.billableCostUsd ?? 0
  const profit = billing.profitUsd ?? (upliftedCost - (billing.rawCostUsd || 0))
  const profitRatio = billing.rawCostUsd > 0 ? profit / billing.rawCostUsd : 0
  const deltaTone = billingDeltaTone(profit)
  const hasLoss = deltaTone === 'loss'
  const hasProfit = deltaTone === 'profit'
  const visiblePools = billingByPool.filter((pool) => pool.requests > 0).slice(0, 20)

  return (
    <Panel
      title="备用池计费拆分"
      subtitle="原始成本来自外部池原始 usage；最终上报使用整形并放大后的 usage；盈利按放大后计费减原始成本计算"
      actions={
        <span className={`rounded border px-2 py-0.5 text-[0.68rem] font-semibold ${
          hasLoss
            ? 'border-error/25 bg-base-100 text-error'
            : hasProfit
              ? 'border-warning/25 bg-base-100 text-warning'
              : 'border-base-300 bg-base-100 text-base-content/55'
        }`}>
          {hasLoss ? `亏损 ${formatUsd(Math.abs(profit))}` : hasProfit ? `盈利 ${formatUsd(profit)}` : '持平'}
        </span>
      }
    >
      <div className="grid gap-3 p-3 md:grid-cols-2 xl:grid-cols-4">
        <div className="rounded-box border border-base-300/60 bg-base-200/40 p-3">
          <div className="text-xs text-base-content/55">外部池请求</div>
          <div className="mt-1 text-lg font-semibold">{formatNumber(billing.requests)}</div>
          <div className="mt-1 text-xs text-base-content/50">可计价 {formatNumber(billing.pricedRequests)} / 未计价 {formatNumber(billing.unpricedRequests)}</div>
        </div>
        <div className="rounded-box border border-base-300/60 bg-base-200/40 p-3">
          <div className="text-xs text-base-content/55">原始成本</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(billing.rawCostUsd)}</div>
          <div className="mt-1 text-xs text-base-content/50">按备用池 raw usage 估算</div>
        </div>
        <div className="rounded-box border border-base-300/60 bg-base-200/40 p-3">
          <div className="text-xs text-base-content/55">整形后计费</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(shapedCost)}</div>
          <div className="mt-1 text-xs text-base-content/50">路径缓存整形后，未放大</div>
        </div>
        <div className="rounded-box border border-base-300/60 bg-base-200/40 p-3">
          <div className="text-xs text-base-content/55">整形后放大计费</div>
          <div className="mt-1 text-lg font-semibold">{formatUsd(upliftedCost)}</div>
          <div className={`mt-1 text-xs ${billingDeltaTextClass(deltaTone)}`}>
            盈利 = 放大后 - 原始：{profit >= 0 ? '+' : ''}{formatUsd(profit)}
          </div>
        </div>
      </div>
      <div className="px-3 pb-3">
        <SignalRow
          label="盈利占原始成本"
          value={`${profit >= 0 ? '+' : ''}${formatUsd(profit)} · ${formatPercent(profitRatio)}`}
          ratio={Math.abs(profitRatio)}
          tone={hasLoss ? 'warning' : 'success'}
        />
      </div>
      <div className="border-t border-base-300/60 px-3 py-3">
        <div className="mb-2 flex items-center justify-between gap-2">
          <div className="text-xs font-semibold text-base-content/70">分号池成本与盈亏</div>
          <div className="text-[0.65rem] text-base-content/45">按当前时间窗口聚合</div>
        </div>
        {visiblePools.length === 0 ? (
          <div className="rounded-box border border-dashed border-base-300 p-3 text-sm text-base-content/45">
            当前窗口没有备用池计费样本。
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="table table-xs">
              <thead>
                <tr>
                  <th>号池</th>
                  <th className="text-right">请求</th>
                  <th className="text-right">原始成本</th>
                  <th className="text-right">整形后</th>
                  <th className="text-right">放大后</th>
                  <th className="text-right">盈亏</th>
                  <th className="text-right">未计价</th>
                  <th className="text-right">兜底</th>
                </tr>
              </thead>
              <tbody>
                {visiblePools.map((pool) => {
                  const poolProfit = pool.profitUsd ?? ((pool.upliftedCostUsd ?? pool.reportedCostUsd ?? 0) - pool.rawCostUsd)
                  const poolTone = billingDeltaTone(poolProfit)
                  return (
                    <tr key={pool.poolId}>
                      <td>
                        <div className="max-w-[220px] truncate font-medium" title={pool.poolName}>{pool.poolName}</div>
                        <div className="font-mono text-[0.62rem] text-base-content/45">#{pool.poolId}</div>
                      </td>
                      <td className="text-right font-mono">{formatNumber(pool.requests)}</td>
                      <td className="text-right font-mono">{formatUsd(pool.rawCostUsd)}</td>
                      <td className="text-right font-mono">{formatUsd(pool.shapedCostUsd ?? pool.reportedCostUsd)}</td>
                      <td className="text-right font-mono">{formatUsd(pool.upliftedCostUsd ?? pool.reportedCostUsd)}</td>
                      <td className={`text-right font-mono ${billingDeltaTextClass(poolTone)}`}>
                        {poolProfit >= 0 ? '+' : ''}{formatUsd(poolProfit)}
                      </td>
                      <td className="text-right font-mono">{formatNumber(pool.unpricedRequests)}</td>
                      <td className="text-right font-mono">{formatNumber(pool.costFloorAppliedRequests)}</td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </Panel>
  )
}

export function UsageDashboardPanel() {
  const autoRefresh = useAutoRefreshPreference(DASHBOARD_AUTO_REFRESH_KEY)
  const dashboard = useUsageDashboard(DASHBOARD_TIMEZONE, autoRefresh.refetchInterval)
  const [selectedWindowKey, setSelectedWindowKey] = useState('today')
  const [rankDimension, setRankDimension] = useState<RankDimension>('credentials')
  const data = dashboard.data
  const selectedWindow = useMemo(
    () => activeWindow(data?.windows || [], selectedWindowKey),
    [data?.windows, selectedWindowKey]
  )

  if (dashboard.isLoading) {
    return <LoadingState text="正在加载总览..." />
  }

  if (dashboard.error) {
    return <ErrorState title="总览加载失败" message={extractErrorMessage(dashboard.error)} />
  }

  if (!data || !selectedWindow) {
    return <EmptyState title="暂无总览数据" description="当前还没有可聚合的请求记录。" />
  }

  const summary = selectedWindow.summary
  const top = data.top || EMPTY_TOP
  const series = data.series || { hourly24h: [], daily7d: [] }
  const externalPoolBilling = summary.externalPoolBilling || EMPTY_EXTERNAL_POOL_BILLING
  const externalPoolBillingByPool = summary.externalPoolBillingByPool || []
  const pricedRatio = summary.totalRequests > 0 ? summary.pricedRequests / summary.totalRequests : 0
  const streamRatio = summary.totalRequests > 0 ? summary.streamRequests / summary.totalRequests : 0
  const totalTokens = summary.totalInputTokens + summary.totalOutputTokens
  const latencyTone: DashboardTone = summary.p95DurationMs >= 60_000 ? 'warning' : 'info'

  return (
    <div className="space-y-4">
      <DashboardToolbar
        data={data}
        selectedWindow={selectedWindow}
        onWindowChange={setSelectedWindowKey}
        autoRefreshEnabled={autoRefresh.enabled}
        autoRefreshSeconds={autoRefresh.intervalSeconds}
        onAutoRefreshEnabledChange={autoRefresh.setEnabled}
        onAutoRefreshSecondsChange={autoRefresh.setIntervalSeconds}
      />

      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-6">
        <MetricTile
          title="请求量"
          value={formatNumber(summary.totalRequests)}
          desc={`成功 ${formatNumber(summary.successRequests)} / 错误 ${formatNumber(summary.errorRequests)}`}
          icon={<Activity className="h-5 w-5" />}
          tone="primary"
        />
        <MetricTile
          title="错误率"
          value={formatPercent(summary.errorRate)}
          desc={summary.errorRequests > 0 ? '需要查看异常摘要' : '当前窗口无错误'}
          icon={summary.errorRequests > 0 ? <ShieldAlert className="h-5 w-5" /> : <CheckCircle2 className="h-5 w-5" />}
          tone={summary.errorRate >= 0.2 ? 'error' : summary.errorRate > 0 ? 'warning' : 'success'}
        />
        <MetricTile
          title="耗时"
          value={`${Math.round(summary.averageDurationMs)}ms`}
          desc={`P95 ${formatNumber(summary.p95DurationMs)}ms`}
          icon={<Clock3 className="h-5 w-5" />}
          tone={latencyTone}
        />
        <MetricTile
          title="估算费用"
          value={formatUsd(summary.totalEstimatedCostUsd)}
          desc={`计价覆盖 ${formatPercent(pricedRatio)}`}
          icon={<DollarSign className="h-5 w-5" />}
          tone="primary"
        />
        <MetricTile
          title="Token"
          value={formatNumber(totalTokens)}
          desc={`输入 ${formatNumber(summary.totalInputTokens)} / 输出 ${formatNumber(summary.totalOutputTokens)}`}
          icon={<BarChart3 className="h-5 w-5" />}
          tone="primary"
        />
        <MetricTile
          title="缓存读取"
          value={formatPercent(summary.cacheReadRatio)}
          desc={`读取 ${formatNumber(summary.totalCacheReadInputTokens)}`}
          icon={<Database className="h-5 w-5" />}
          tone="success"
        />
      </div>

      <div className="grid gap-3 xl:grid-cols-2">
        <SeriesChart title="最近 24 小时请求趋势" points={series.hourly24h || []} />
        <SeriesChart title="最近 7 天请求趋势" points={series.daily7d || []} />
      </div>

      <div className="grid gap-3 xl:grid-cols-[1.1fr_0.9fr]">
        <OperationsPanel
          pricedRatio={pricedRatio}
          streamRatio={streamRatio}
          cacheReadRatio={summary.cacheReadRatio}
          stickyBoundRequests={summary.stickyBoundRequests}
          fallbackFromStickyRequests={summary.fallbackFromStickyRequests}
          simulatedRequests={summary.simulatedRequests}
          upstreamMetadataRequests={summary.upstreamMetadataRequests}
        />
        <ErrorFocusPanel totalErrors={summary.errorRequests} items={top.errors || []} />
      </div>

      <ExternalPoolBillingPanel billing={externalPoolBilling} billingByPool={externalPoolBillingByPool} />

      <div className="grid gap-3 xl:grid-cols-2">
        <BreakdownPanel title="状态分布" subtitle="判断成功、上游超时、客户端错误等整体占比" items={summary.statusBreakdown || []} emptyText="暂无状态样本。" />
        <BreakdownPanel title="用量来源" subtitle="判断真实上游、缓存模拟、补录等来源占比" items={summary.usageSourceBreakdown || []} emptyText="暂无来源样本。" />
      </div>

      <DimensionRankPanel top={top} activeKey={rankDimension} onActiveKeyChange={setRankDimension} />

      <div className="rounded-box border border-base-300/60 bg-base-100 px-3 py-2.5 text-xs text-base-content/50">
        <div className="flex flex-wrap items-center gap-2">
          <Gauge className="h-4 w-4 text-base-content/35" />
          <span>总览保留 Top 维度聚合；单条请求链路和更精确筛选请在“用量”页查看。</span>
          <LineChart className="h-4 w-4 text-base-content/35" />
          <span>{autoRefresh.enabled ? `页面数据每 ${autoRefresh.intervalSeconds} 秒自动刷新。` : '自动刷新已关闭。'}</span>
          {summary.errorRequests > 0 && (
            <>
              <AlertTriangle className={`h-4 w-4 ${summary.errorRate >= 0.2 ? 'text-error' : 'text-warning'}`} />
              <span className={summary.errorRate >= 0.2 ? 'text-error' : 'text-warning'}>
                当前窗口存在错误请求，优先查看异常摘要和用量详情。
              </span>
            </>
          )}
          {summary.fallbackFromStickyRequests > 0 && (
            <>
              <Zap className="h-4 w-4 text-warning" />
              <span className="text-warning">检测到 Sticky 回退，说明粘度命中的账号不可用或并发不可用。</span>
            </>
          )}
        </div>
      </div>
    </div>
  )
}
