import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  DollarSign,
  Layers3,
  RefreshCw,
  TrendingUp,
  Wallet,
  Zap,
} from 'lucide-react'
import { getAccounts, getAccountsStatus } from '@/api/accounts'
import {
  useUsageOverview,
  useUsageOverviewRankings,
  useUsageOverviewSeries,
  useUsageOverviewSummary,
  useUsageSummary,
} from '@/hooks/use-usage'
import { useAutoRefreshPreference } from '@/hooks/use-auto-refresh'
import { cacheHitRate, cacheHitRateLevel, type CacheHitRateLevel } from './cache-hit-rate'
import { accountUsageTier, compareUsageTier, type UsageTier } from './usage-order'
import {
  accountCostDelta,
  costDeltaPercent,
  costMultiplier,
  localDateTimeToIso,
  localDateTimeValue,
  metricsShare,
  totalTokenUsage,
} from './overview-metrics'
import { OverviewTrendCard, accountTrendViews, summaryTrendViews } from './overview-trends'
import { billingDeltaTextClass, billingDeltaTone } from '@/features/usage/usage-helpers'
import { formatCompact, formatDate, formatNumber, formatPercent, formatUsdFixed2 } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import type {
  Account,
  AccountStatus,
  UsageOverviewMetrics,
  UsageOverviewPoolRow,
  UsageOverviewRankRow,
  UsageOverviewSeriesPoint,
} from '@/types/api'
import {
  Callout,
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Input,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui'

const TIMEZONE = 'Asia/Shanghai'
const AUTO_REFRESH_KEY = 'claude-pool-admin:auto-refresh:overview'

type PresetRange = 'today' | 'yesterday' | 'last24h' | 'last7d' | 'last30d' | 'thisMonth'
type OverviewTab = 'summary' | 'accounts' | 'rankings'
type RankingDimension = 'models' | 'accounts' | 'keys' | 'paths' | 'errors'

const PRESETS: Array<{ key: PresetRange; label: string }> = [
  { key: 'today', label: '今天' },
  { key: 'yesterday', label: '昨天' },
  { key: 'last24h', label: '24 小时' },
  { key: 'last7d', label: '7 天' },
  { key: 'last30d', label: '30 天' },
  { key: 'thisMonth', label: '本月' },
]

const EMPTY_OVERVIEW_METRICS: UsageOverviewMetrics = {
  requests: 0,
  successRequests: 0,
  errorRequests: 0,
  errorRate: 0,
  inputTokens: 0,
  uncachedInputTokens: 0,
  outputTokens: 0,
  cacheReadInputTokens: 0,
  cacheCreationInputTokens: 0,
  estimatedCostUsd: 0,
  originalCostUsd: 0,
  pricedRequests: 0,
  unpricedRequests: 0,
  averageDurationMs: 0,
}

const HIT_RATE_TONE: Record<CacheHitRateLevel, 'success' | 'warning' | 'error' | 'default'> = {
  ok: 'success',
  warning: 'warning',
  critical: 'error',
  none: 'default',
}

const HIT_RATE_TEXT: Record<CacheHitRateLevel, string> = {
  ok: 'text-foreground',
  warning: 'text-warning',
  critical: 'text-destructive',
  none: 'text-muted-foreground',
}

const HIT_RATE_HINT: Record<CacheHitRateLevel, string> = {
  ok: '',
  warning: '命中率偏低，需关注',
  critical: '命中率过低',
  none: '没有输入 token',
}

const USAGE_TIER_BADGE: Record<UsageTier, { label: string; tone: 'success' | 'info' | 'warning' | 'neutral' }> = {
  active: { label: '使用中', tone: 'success' },
  ready: { label: '可用', tone: 'info' },
  cooling: { label: '冷却中', tone: 'warning' },
  unavailable: { label: '不可用', tone: 'neutral' },
}

function rangeLabel(range: { key?: string | null; from: string; to: string }): string {
  if (range.key) return PRESETS.find((item) => item.key === range.key)?.label ?? range.key
  return `${formatDate(range.from)} - ${formatDate(range.to)}`
}

function formatHitRate(rate: number | null): string {
  return rate == null ? '-' : formatPercent(rate)
}

/** 跳到用量页，带上账号与时间范围筛选。 */
function usageLink(
  navigate: ReturnType<typeof useNavigate>,
  params: { accountId?: number; endpoint?: string; since: string; until: string },
) {
  const search = new URLSearchParams()
  if (params.accountId) search.set('accountId', String(params.accountId))
  if (params.endpoint) search.set('endpoint', params.endpoint)
  if (params.since) search.set('since', localDateTimeValue(params.since))
  if (params.until) search.set('until', localDateTimeValue(params.until))
  navigate(`/usage?${search.toString()}`)
}

// ─── 小组件 ─────────────────────────────────────────────────────────────────────

function HitRateCell({ metrics }: { metrics: UsageOverviewMetrics }) {
  const rate = cacheHitRate(metrics)
  const level = cacheHitRateLevel(rate)
  return (
    <span
      className={cn('font-mono tabular-nums', HIT_RATE_TEXT[level])}
      title={HIT_RATE_HINT[level] || '缓存读 ÷（未缓存输入 + 缓存读 + 缓存写）'}
    >
      {formatHitRate(rate)}
    </span>
  )
}

function UsageTierBadge({ tier, inFlight }: { tier: UsageTier; inFlight?: number }) {
  const badge = USAGE_TIER_BADGE[tier]
  const label = tier === 'active' && inFlight ? `${badge.label} ${inFlight}` : badge.label
  return <Badge tone={badge.tone} size="xs" title={tier === 'active' ? `当前在途请求 ${inFlight ?? 0} 个` : undefined}>{label}</Badge>
}

function TokenSummaryCard({ metrics, className }: { metrics: UsageOverviewMetrics; className?: string }) {
  const total = totalTokenUsage(metrics)
  const items = [
    { label: '输入', value: metrics.inputTokens, className: 'text-emerald-600 dark:text-emerald-400' },
    { label: '输出', value: metrics.outputTokens, className: 'text-violet-600 dark:text-violet-400' },
    { label: '缓存读', value: metrics.cacheReadInputTokens, className: 'text-sky-600 dark:text-sky-400' },
    { label: '缓存写', value: metrics.cacheCreationInputTokens, className: 'text-amber-600 dark:text-amber-400' },
  ]
  return (
    <div className={cn(
      'relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl bg-card p-4 shadow-sm transition hover:shadow-md',
      className,
    )}>
      <span className="absolute left-0 top-4 h-8 w-1 rounded-r-full bg-amber-500" />
      <div className="flex items-start justify-between gap-2 pl-2.5">
        <div className="min-w-0 flex-1">
          <div className="text-[0.72rem] font-semibold text-muted-foreground">Token</div>
          <div className="mt-1 break-words text-2xl font-semibold tracking-tight tabular-nums text-amber-600 dark:text-amber-400" title={formatNumber(total)}>
            {formatCompact(total)}
          </div>
        </div>
        <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-amber-600 dark:text-amber-400 [&_svg]:size-4">
          <Layers3 />
        </div>
      </div>
      <div className="mt-3 grid grid-cols-2 gap-x-4 gap-y-1.5 pl-2.5 text-[0.72rem] leading-4 sm:grid-cols-4">
        {items.map((item) => (
          <div key={item.label} className="min-w-0">
            <div className="text-muted-foreground">{item.label}</div>
            <div className={cn('truncate font-semibold tabular-nums', item.className)} title={formatNumber(item.value)}>
              {formatCompact(item.value)}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

// ─── 汇总分区 ───────────────────────────────────────────────────────────────────

type BillingTotals = {
  rawCostUsd: number
  shapedCostUsd: number
  upliftedCostUsd: number
  reportedCostUsd: number
  billableCostUsd: number
  profitUsd: number
  costFloorDeltaUsd: number
  costFloorAppliedRequests: number
}

function OverallSummarySection({
  metrics,
  billing,
  accountCostUsd,
  rawTokens,
  realtime,
}: {
  metrics: UsageOverviewMetrics
  billing: BillingTotals
  accountCostUsd: number
  rawTokens: number
  realtime?: { windowSeconds: number; rpm: number; successRequests?: number; errorRequests?: number; totalTpm: number }
}) {
  const hitRate = cacheHitRate(metrics)
  const hitLevel = cacheHitRateLevel(hitRate)
  const delta = accountCostDelta(billing.reportedCostUsd, accountCostUsd)
  const reportedTokens = totalTokenUsage(metrics)
  return (
    <SectionCard>
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard
          title="请求数"
          value={formatCompact(metrics.requests)}
          valueTitle={formatNumber(metrics.requests)}
          desc={`${formatCompact(metrics.successRequests)} 成功 · ${formatCompact(metrics.errorRequests)} 错误`}
          icon={<Activity />}
          tone="primary"
        />
        <StatCard
          title="成功率"
          value={formatPercent(metricsShare(metrics.successRequests, metrics.requests))}
          desc={`错误率 ${formatPercent(metrics.errorRate)} · 平均耗时 ${Math.round(metrics.averageDurationMs)} ms`}
          icon={<TrendingUp />}
          tone={metrics.errorRate > 0.05 ? 'warning' : 'success'}
        />
        <StatCard
          title="实时 RPM"
          value={formatNumber(realtime?.rpm ?? 0)}
          desc={realtime
            ? `近 ${realtime.windowSeconds} 秒 · 成功 ${formatNumber(realtime.successRequests ?? 0)} · 错误 ${formatNumber(realtime.errorRequests ?? 0)} · TPM ${formatCompact(realtime.totalTpm)}`
            : '实时统计加载中'}
          icon={<Zap />}
          tone="info"
        />
        <StatCard
          title="缓存命中率"
          value={formatHitRate(hitRate)}
          desc={HIT_RATE_HINT[hitLevel] || '缓存读 ÷（未缓存输入 + 缓存读 + 缓存写）'}
          icon={<Layers3 />}
          tone={HIT_RATE_TONE[hitLevel]}
        />
        <TokenSummaryCard metrics={metrics} className="sm:col-span-2" />
        <StatCard
          title="上游原始量"
          value={formatCompact(rawTokens)}
          valueTitle={formatNumber(rawTokens)}
          desc={`报告量 ${formatCompact(reportedTokens)} · 原始/报告 ${reportedTokens > 0 ? (rawTokens / reportedTokens).toFixed(3) : '-'}`}
          icon={<Layers3 />}
          tone="default"
        />
        <StatCard
          title="估算计费"
          value={formatUsdFixed2(billing.reportedCostUsd)}
          desc={`按最终上报 usage 计价 · 计价覆盖 ${formatPercent(metricsShare(metrics.pricedRequests, metrics.requests))}`}
          icon={<DollarSign />}
          tone="primary"
        />
        <StatCard
          title="原始计费"
          value={formatUsdFixed2(billing.rawCostUsd)}
          desc="按上游原始 usage 计价"
          icon={<DollarSign />}
          tone="warning"
        />
        <StatCard
          title="账号成本"
          value={formatUsdFixed2(accountCostUsd)}
          desc="上游原始成本 × 各账号成本倍率"
          icon={<Wallet />}
          tone="warning"
        />
        <StatCard
          title="差额"
          value={`${delta.deltaUsd >= 0 ? '+' : '-'}${formatUsdFixed2(Math.abs(delta.deltaUsd))}`}
          desc={`估算计费 - 账号成本 · 占账号成本 ${delta.percentText}`}
          icon={<TrendingUp />}
          tone={delta.tone === 'loss' ? 'error' : delta.tone === 'profit' ? 'success' : 'default'}
        />
        <StatCard
          title="可计费"
          value={formatUsdFixed2(billing.billableCostUsd)}
          desc={(
            <span>
              补偿后计费 {formatUsdFixed2(billing.upliftedCostUsd)} · 毛利{' '}
              <span className={billingDeltaTextClass(billingDeltaTone(billing.profitUsd))}>
                {billing.profitUsd >= 0 ? '+' : '-'}{formatUsdFixed2(Math.abs(billing.profitUsd))}
              </span>
            </span>
          )}
          icon={<DollarSign />}
          tone={billing.profitUsd < 0 ? 'error' : 'success'}
        />
      </div>
      <div className="mt-3 rounded-lg bg-muted/25 px-3 py-2 text-xs text-muted-foreground">
        <span className="font-medium text-foreground/75">估算计费</span> 按最终上报给下游的 usage 计价，是对下游收的钱；
        <span className="ml-1 font-medium text-foreground/75">原始计费</span> 按上游返回的原始 usage 计价；
        <span className="ml-1 font-medium text-foreground/75">账号成本</span> 是上游原始成本乘以各账号的成本倍率，差额即估算计费减账号成本。
        <span className="ml-1 font-medium text-foreground/75">可计费</span> 是补偿与成本底线修复后的最终金额，毛利按它减上游原始成本。
      </div>
    </SectionCard>
  )
}

// ─── 账号分区 ───────────────────────────────────────────────────────────────────

function AccountsSection({
  billing,
  accounts,
  statuses,
  range,
  navigate,
}: {
  billing: BillingTotals & { pools: UsageOverviewPoolRow[] }
  accounts: Account[]
  statuses: AccountStatus[]
  range: { from: string; to: string }
  navigate: ReturnType<typeof useNavigate>
}) {
  const usageById = new Map(billing.pools.map((item) => [item.poolId, item]))
  const statusById = new Map(statuses.map((item) => [item.account.id, item]))
  const configuredIds = new Set(accounts.map((account) => account.id))
  const rows = [
    ...accounts.map((account) => ({
      account,
      accountId: account.id,
      name: account.name,
      priority: account.priority,
      usage: usageById.get(account.id),
      status: statusById.get(account.id),
      tier: accountUsageTier(account, statusById.get(account.id)),
    })),
    ...billing.pools
      .filter((usage) => !configuredIds.has(usage.poolId))
      .map((usage) => ({
        account: undefined,
        accountId: usage.poolId,
        name: usage.poolName || `#${usage.poolId}`,
        priority: Number.MAX_SAFE_INTEGER,
        usage,
        status: undefined,
        tier: accountUsageTier(undefined, undefined),
      })),
  ]
    // 正在使用的账号排最前；同一层内按可计费金额排序，避免刷新时行序乱跳。
    .sort((left, right) => (
      compareUsageTier(left.tier, right.tier) ||
      (right.usage?.billableCostUsd ?? 0) - (left.usage?.billableCostUsd ?? 0) ||
      (right.usage?.metrics.requests ?? 0) - (left.usage?.metrics.requests ?? 0) ||
      left.priority - right.priority ||
      left.accountId - right.accountId
    ))
  const totalRequests = rows.reduce((sum, item) => sum + (item.usage?.metrics.requests ?? 0), 0)
  const pricedRequests = rows.reduce((sum, item) => sum + (item.usage?.metrics.pricedRequests ?? 0), 0)
  const unpricedRequests = rows.reduce((sum, item) => sum + (item.usage?.metrics.unpricedRequests ?? 0), 0)
  const deltaTone = billingDeltaTone(billing.profitUsd)
  return (
    <SectionCard>
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard
          title="账号请求"
          value={formatNumber(totalRequests)}
          desc={`可计价 ${formatNumber(pricedRequests)} / 未计价 ${formatNumber(unpricedRequests)}`}
          icon={<Zap />}
          tone="info"
        />
        <StatCard title="上游原始成本" value={formatUsdFixed2(billing.rawCostUsd)} icon={<DollarSign />} tone="warning" />
        <StatCard
          title="展示计费"
          value={formatUsdFixed2(billing.shapedCostUsd)}
          desc="整形后的中间成本口径"
          icon={<DollarSign />}
          tone="primary"
        />
        <StatCard
          title="可计费"
          value={formatUsdFixed2(billing.billableCostUsd)}
          desc={(
            <span>
              补偿后计费 {formatUsdFixed2(billing.upliftedCostUsd)} ·{' '}
              <span className={billingDeltaTextClass(deltaTone)}>
                {billing.profitUsd >= 0 ? '+' : '-'}{formatUsdFixed2(Math.abs(billing.profitUsd))}
              </span>
              {billing.costFloorAppliedRequests > 0 && ` · 兜底 ${formatNumber(billing.costFloorAppliedRequests)} 次`}
            </span>
          )}
          icon={<TrendingUp />}
          tone={deltaTone === 'loss' ? 'error' : deltaTone === 'profit' ? 'warning' : 'success'}
        />
      </div>
      <div className="mt-3 rounded-lg bg-muted/25 px-3 py-2 text-xs text-muted-foreground">
        <span className="font-medium text-foreground/75" title="按整形后的展示 usage 计算的中间成本">展示计费</span>
        {' '}是整形后的中间口径；
        <span className="ml-1 font-medium text-foreground/75" title="按最终上报 usage 计算，包含输出补偿和成本底线修复">补偿后计费</span>
        {' '}是最终上报口径，可计费取最终可计费金额。
      </div>
      {rows.length === 0 ? (
        <EmptyState title="暂无账号" className="mt-3 py-8" />
      ) : (
        <div className="mt-4 scrollbar-thin overflow-x-auto">
          <Table className="min-w-[1120px] text-xs">
            <TableHeader>
              <TableRow>
                <TableHead>账号</TableHead>
                <TableHead className="text-right">请求</TableHead>
                <TableHead className="text-right">错误率</TableHead>
                <TableHead className="text-right">上游原始成本</TableHead>
                <TableHead className="text-right" title="按整形后的展示 usage 计算的中间成本">展示计费</TableHead>
                <TableHead className="text-right" title="按最终上报 usage 计算，包含输出补偿和成本底线修复">补偿后计费</TableHead>
                <TableHead className="text-right">差额占原始</TableHead>
                <TableHead className="text-right">原始/整形倍率</TableHead>
                <TableHead className="text-right" title="缓存读 ÷（未缓存输入 + 缓存读 + 缓存写）">缓存命中率</TableHead>
                <TableHead className="text-right">未计价</TableHead>
                <TableHead className="text-right">兜底</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map(({ account, accountId, name, usage, status, tier }) => {
                const metrics = usage?.metrics ?? EMPTY_OVERVIEW_METRICS
                const rawCostUsd = usage?.rawCostUsd ?? 0
                const shapedCostUsd = usage?.shapedCostUsd ?? 0
                const upliftedCostUsd = usage?.upliftedCostUsd ?? 0
                const rowDelta = usage?.profitUsd ?? (upliftedCostUsd - rawCostUsd)
                const rowTone = billingDeltaTone(rowDelta)
                return (
                  <TableRow key={accountId}>
                    <TableCell>
                      <div className="flex min-w-[200px] flex-wrap items-center gap-1.5">
                        <Button
                          variant="link"
                          size="xs"
                          className="h-auto max-w-[260px] justify-start truncate px-0 text-left text-xs font-semibold"
                          title={`账号 #${accountId} · 查看用量明细`}
                          onClick={() => usageLink(navigate, { accountId, since: range.from, until: range.to })}
                        >
                          #{accountId} {name}
                        </Button>
                        <UsageTierBadge tier={tier} inFlight={status?.inFlight} />
                        {account && <Badge tone={account.enabled ? 'success' : 'neutral'} size="xs">{account.enabled ? '启用' : '停用'}</Badge>}
                        {account?.autoDisabled && <Badge tone="error" size="xs">自动禁用</Badge>}
                        {!account && <Badge tone="neutral" size="xs">已删除</Badge>}
                        {metrics.requests === 0 && <Badge tone="secondary" size="xs">无流量</Badge>}
                      </div>
                    </TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(metrics.requests)}>{formatNumber(metrics.requests)}</TableCell>
                    <TableCell className={cn('text-right font-mono', metrics.errorRate > 0.05 && 'text-destructive')}>{formatPercent(metrics.errorRate)}</TableCell>
                    <TableCell className="text-right font-mono">{formatUsdFixed2(rawCostUsd)}</TableCell>
                    <TableCell className="text-right font-mono" title="整形后的中间成本口径">{formatUsdFixed2(shapedCostUsd)}</TableCell>
                    <TableCell className="text-right font-mono" title="最终上报/补偿后成本口径；当前用于可计费金额">{formatUsdFixed2(upliftedCostUsd)}</TableCell>
                    <TableCell className={cn('text-right font-mono', billingDeltaTextClass(rowTone))}>
                      <div>{rowDelta >= 0 ? '+' : ''}{formatUsdFixed2(rowDelta)}</div>
                      <div className="text-[0.62rem]" title="当前账号差额 ÷ 当前账号上游原始成本">{costDeltaPercent(rowDelta, rawCostUsd)}</div>
                    </TableCell>
                    <TableCell className="text-right font-mono" title="上游原始成本 ÷ 整形后展示成本">{costMultiplier(rawCostUsd, shapedCostUsd)}</TableCell>
                    <TableCell className="text-right"><HitRateCell metrics={metrics} /></TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(metrics.unpricedRequests)}>{formatNumber(metrics.unpricedRequests)}</TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(usage?.costFloorAppliedRequests ?? 0)}>{formatNumber(usage?.costFloorAppliedRequests ?? 0)}</TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </SectionCard>
  )
}

// ─── 排行分区 ───────────────────────────────────────────────────────────────────

function RankPanel({ rows, total, errorRank = false }: { rows: UsageOverviewRankRow[]; total: number; errorRank?: boolean }) {
  return (
    <div className="mt-3">
      {rows.length === 0 ? (
        <EmptyState title="暂无排行数据" className="py-8" />
      ) : (
        <Table className="min-w-[560px]">
          <TableHeader>
            <TableRow>
              <TableHead className="w-10">#</TableHead>
              <TableHead>名称</TableHead>
              <TableHead className="text-right">{errorRank ? '错误数' : '请求数'}</TableHead>
              <TableHead className="text-right">{errorRank ? '占错误' : '占请求'}</TableHead>
              <TableHead className="text-right">估算计费</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, index) => (
              <TableRow key={row.key}>
                <TableCell className="font-mono text-xs text-muted-foreground">{index + 1}</TableCell>
                <TableCell>
                  <div className="max-w-[250px] truncate text-xs font-semibold" title={row.label}>{row.label}</div>
                  {row.label !== row.key && <div className="max-w-[250px] truncate font-mono text-[0.62rem] text-muted-foreground/60">{row.key}</div>}
                </TableCell>
                <TableCell className="text-right font-mono text-xs">{formatCompact(errorRank ? row.errorRequests : row.requests)}</TableCell>
                <TableCell className="text-right font-mono text-xs">{formatPercent(metricsShare(errorRank ? row.errorRequests : row.requests, total))}</TableCell>
                <TableCell className="text-right font-mono text-xs">{formatUsdFixed2(row.estimatedCostUsd)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  )
}

function RankingsSection({
  rankings,
}: {
  rankings: {
    totalRequests: number
    totalErrors: number
    topModels: UsageOverviewRankRow[]
    topAccounts: UsageOverviewRankRow[]
    topKeys: UsageOverviewRankRow[]
    topPaths: UsageOverviewRankRow[]
    topErrors: UsageOverviewRankRow[]
  }
}) {
  const [dimension, setDimension] = useState<RankingDimension>('models')
  const rankViews: Array<{ key: RankingDimension; label: string; rows: UsageOverviewRankRow[]; total: number; errorRank?: boolean }> = [
    { key: 'models', label: '模型', rows: rankings.topModels, total: rankings.totalRequests },
    { key: 'accounts', label: '账号', rows: rankings.topAccounts, total: rankings.totalRequests },
    { key: 'keys', label: '密钥', rows: rankings.topKeys, total: rankings.totalRequests },
    { key: 'paths', label: '路由', rows: rankings.topPaths, total: rankings.totalRequests },
    { key: 'errors', label: '错误', rows: rankings.topErrors, total: rankings.totalErrors, errorRank: true },
  ]
  return (
    <SectionCard>
      <Tabs value={dimension} onValueChange={(value) => setDimension(value as RankingDimension)}>
        <TabsList className="w-full justify-start overflow-x-auto">
          {rankViews.map((view) => <TabsTrigger key={view.key} value={view.key}>{view.label}</TabsTrigger>)}
        </TabsList>
        {rankViews.map((view) => (
          <TabsContent key={view.key} value={view.key}>
            <RankPanel rows={view.rows} total={view.total} errorRank={view.errorRank} />
          </TabsContent>
        ))}
      </Tabs>
    </SectionCard>
  )
}

// ─── 页面 ───────────────────────────────────────────────────────────────────────

export function OverviewPage() {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const autoRefresh = useAutoRefreshPreference(AUTO_REFRESH_KEY, 30)

  const tabParam = searchParams.get('tab')
  const activeTab: OverviewTab = tabParam === 'accounts' || tabParam === 'rankings' ? tabParam : 'summary'

  const rangeParam = searchParams.get('range')
  const isPreset = PRESETS.some((item) => item.key === rangeParam)
  const rangeKey = (isPreset ? rangeParam : undefined) as PresetRange | undefined
  const fromParam = searchParams.get('from') ?? ''
  const toParam = searchParams.get('to') ?? ''
  const [customFrom, setCustomFrom] = useState(() => localDateTimeValue(fromParam))
  const [customTo, setCustomTo] = useState(() => localDateTimeValue(toParam))
  const [customError, setCustomError] = useState('')

  const hourlyParam = searchParams.get('granularity') === 'hour'
  const queryParams = useMemo(() => {
    if (rangeKey) {
      // 7 天范围允许显式切到按小时；其余预设沿用后端默认粒度。
      return rangeKey === 'last7d' && hourlyParam
        ? { timezone: TIMEZONE, range: rangeKey, granularity: 'hour' as const }
        : { timezone: TIMEZONE, range: rangeKey }
    }
    if (fromParam && toParam) {
      return { timezone: TIMEZONE, from: fromParam, to: toParam, granularity: searchParams.get('granularity') === 'hour' ? 'hour' as const : 'day' as const }
    }
    return { timezone: TIMEZONE, range: 'today' }
  }, [fromParam, hourlyParam, rangeKey, searchParams, toParam])
  const rankingQueryParams = useMemo(() => ({ ...queryParams, topN: 50 as number }), [queryParams])

  // 只有包含“现在”的范围才自动刷新；昨天、固定区间不会再变。
  const refreshInterval = (!rangeKey && !fromParam) || (rangeKey && ['today', 'last24h', 'last7d', 'last30d', 'thisMonth'].includes(rangeKey))
    ? autoRefresh.refetchInterval
    : false

  const summary = useUsageOverviewSummary(queryParams, refreshInterval)
  // 全量接口额外带账号成本与上游原始量（汇总接口没有）；按账号拆分的计费表也在这里。
  const overview = useUsageOverview(queryParams, refreshInterval, !summary.isError)
  const series = useUsageOverviewSeries(queryParams, refreshInterval, (activeTab === 'summary' || activeTab === 'accounts') && !summary.isError)
  const rankings = useUsageOverviewRankings(rankingQueryParams, refreshInterval, activeTab === 'rankings' && !summary.isError)
  // 实时 RPM 来自进程内最近 60 秒窗口，与时间范围无关。
  const usageSummary = useUsageSummary(autoRefresh.refetchInterval || 30000)
  const accounts = useQuery({ queryKey: ['accounts'], queryFn: getAccounts, enabled: activeTab === 'accounts', refetchInterval: refreshInterval || 30000 })
  const accountStatuses = useQuery({ queryKey: ['accounts-status'], queryFn: getAccountsStatus, enabled: activeTab === 'accounts', refetchInterval: 5000 })

  const setOverviewTab = (tab: OverviewTab) => {
    const next = new URLSearchParams(searchParams)
    if (tab === 'summary') next.delete('tab')
    else next.set('tab', tab)
    setSearchParams(next)
  }

  const applyPreset = (key: PresetRange) => {
    const next = new URLSearchParams(searchParams)
    for (const param of ['from', 'to', 'granularity', 'backRange', 'backFrom', 'backTo']) next.delete(param)
    next.set('range', key)
    setSearchParams(next)
    setCustomError('')
  }

  const applyCustom = () => {
    const from = localDateTimeToIso(customFrom)
    const to = localDateTimeToIso(customTo)
    if (!from || !to || new Date(from) >= new Date(to)) {
      setCustomError('请输入有效的起止时间，且起点必须早于终点。')
      return
    }
    const next = new URLSearchParams(searchParams)
    for (const param of ['range', 'backRange', 'backFrom', 'backTo']) next.delete(param)
    next.set('from', from)
    next.set('to', to)
    next.set('granularity', 'day')
    setSearchParams(next)
    setCustomError('')
  }

  // 只有按天的柱子能下钻到当天的小时；小时已经是最细粒度。
  const seriesGranularity = series.data?.range.granularity ?? summary.data?.range.granularity
  const drillDown = (point: UsageOverviewSeriesPoint) => {
    const current = new URLSearchParams(searchParams)
    const start = new Date(point.bucketStart)
    const end = new Date(start)
    end.setUTCDate(end.getUTCDate() + 1)
    const next = new URLSearchParams(searchParams)
    next.set('from', start.toISOString())
    next.set('to', end.toISOString())
    next.set('granularity', 'hour')
    if (current.get('range')) next.set('backRange', current.get('range') as string)
    if (current.get('from')) next.set('backFrom', current.get('from') as string)
    if (current.get('to')) next.set('backTo', current.get('to') as string)
    next.delete('range')
    setSearchParams(next)
  }

  const setHourly = (hourly: boolean) => {
    const next = new URLSearchParams(searchParams)
    if (hourly) next.set('granularity', 'hour')
    else next.delete('granularity')
    setSearchParams(next)
  }

  const returnFromDrillDown = () => {
    const next = new URLSearchParams(searchParams)
    const backRange = searchParams.get('backRange')
    const backFrom = searchParams.get('backFrom')
    const backTo = searchParams.get('backTo')
    for (const param of ['backRange', 'backFrom', 'backTo']) next.delete(param)
    if (backRange) {
      next.set('range', backRange)
      for (const param of ['from', 'to', 'granularity']) next.delete(param)
    } else if (backFrom && backTo) {
      next.set('from', backFrom)
      next.set('to', backTo)
      next.set('granularity', 'day')
    } else next.set('range', 'today')
    setSearchParams(next)
  }

  const summaryData = summary.data
  const overviewData = overview.data
  const seriesPoints = series.data?.series ?? []
  const seriesGroups = series.data?.seriesGroups ?? { accounts: [] }
  // 还没结束的时段（当前小时或当天）：柱子半透明并在提示里标“进行中”。
  const dataThroughMs = Date.parse(series.data?.range.dataThrough ?? '')
  const bucketMs = seriesGranularity === 'day' ? 86_400_000 : 3_600_000
  const firstPartial = seriesPoints.findIndex((point) => Date.parse(point.bucketStart) + bucketMs > dataThroughMs)
  const partialFrom = firstPartial < 0 ? seriesPoints.length : firstPartial
  const isDrilled = Boolean(searchParams.get('backRange') || searchParams.get('backFrom'))
  const partialErrors = [
    overview.error ? `账号成本：${extractErrorMessage(overview.error)}` : '',
    series.error ? `趋势：${extractErrorMessage(series.error)}` : '',
    usageSummary.error ? `实时统计：${extractErrorMessage(usageSummary.error)}` : '',
    accounts.error ? `账号列表：${extractErrorMessage(accounts.error)}` : '',
    accountStatuses.error ? `账号状态：${extractErrorMessage(accountStatuses.error)}` : '',
    rankings.error ? `排行：${extractErrorMessage(rankings.error)}` : '',
  ].filter(Boolean)

  const refetchAll = () => {
    void summary.refetch()
    void overview.refetch()
    void series.refetch()
    void usageSummary.refetch()
    if (activeTab === 'accounts') {
      void accounts.refetch()
      void accountStatuses.refetch()
    }
    if (activeTab === 'rankings') void rankings.refetch()
  }

  const headerActions = (
    <div className="flex w-full flex-wrap items-center justify-between gap-2">
      <div className="flex flex-wrap items-center gap-1.5">
        {PRESETS.map((item) => (
          <Button
            key={item.key}
            size="sm"
            variant={rangeKey === item.key || (!rangeKey && !fromParam && item.key === 'today') ? 'default' : 'outline'}
            onClick={() => applyPreset(item.key)}
          >
            {item.label}
          </Button>
        ))}
        {isDrilled && (
          <Button size="sm" variant="secondary" onClick={returnFromDrillDown}>
            <ArrowLeft className="size-3.5" />返回整体范围
          </Button>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <span>自定义</span>
          <Input type="datetime-local" value={customFrom} onChange={(event) => setCustomFrom(event.target.value)} className="h-8 w-[172px] text-xs" />
          <span>至</span>
          <Input type="datetime-local" value={customTo} onChange={(event) => setCustomTo(event.target.value)} className="h-8 w-[172px] text-xs" />
          <Button size="sm" variant="outline" onClick={applyCustom}>应用</Button>
        </label>
        <label className="flex cursor-pointer select-none items-center gap-2 text-xs text-muted-foreground">
          <Switch checked={autoRefresh.enabled} onCheckedChange={autoRefresh.setEnabled} />
          自动刷新
        </label>
        <Button size="icon-sm" variant="ghost" title="刷新总览" onClick={refetchAll}>
          <RefreshCw className={cn('size-4', summary.isFetching && 'animate-spin')} />
        </Button>
      </div>
    </div>
  )

  if (summary.isLoading && !summaryData) {
    return (
      <PageContainer>
        <PageHeader title="总览" actions={headerActions} />
        <LoadingState text="加载总览..." />
      </PageContainer>
    )
  }

  if (summary.error || !summaryData) {
    return (
      <PageContainer>
        <PageHeader title="总览" actions={headerActions} />
        <ErrorState title="总览加载失败" message={extractErrorMessage(summary.error)} action={<Button size="sm" onClick={() => void summary.refetch()}>重试</Button>} />
      </PageContainer>
    )
  }

  const billing: BillingTotals & { pools: UsageOverviewPoolRow[] } = {
    ...summaryData.external,
    pools: overviewData?.billing.pools ?? [],
  }

  return (
    <PageContainer>
      <PageHeader title="总览" actions={headerActions} />

      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <span className="font-semibold text-foreground">{rangeLabel(summaryData.range)}</span>
        <span>·</span>
        <span>数据截至 {formatDate(summaryData.range.dataThrough)}</span>
        <span>·</span>
        <span>范围已按整点对齐：{formatDate(summaryData.range.from)} - {formatDate(summaryData.range.to)}</span>
        {summaryData.range.granularity === 'day' && <Badge tone="neutral">按天聚合</Badge>}
        {rangeKey === 'last7d' && (
          <Button size="xs" variant="outline" className="h-6 px-2 text-[0.68rem]" onClick={() => setHourly(!hourlyParam)}>
            {hourlyParam ? '改为按天' : '改为按小时'}
          </Button>
        )}
        {refreshInterval && <Badge tone="success">每 {autoRefresh.intervalSeconds} 秒刷新</Badge>}
      </div>

      {customError && <Callout tone="warning">{customError}</Callout>}
      {partialErrors.length > 0 && <Callout tone="warning">部分分区加载失败：{partialErrors.join('；')}</Callout>}
      {summaryData.totals.all.errorRequests > 0 && summaryData.totals.all.errorRate >= 0.05 && (
        <Callout tone={summaryData.totals.all.errorRate >= 0.2 ? 'error' : 'warning'}>
          <AlertTriangle className="mr-1 inline size-3.5" />
          当前范围错误率 {formatPercent(summaryData.totals.all.errorRate)}，共 {formatNumber(summaryData.totals.all.errorRequests)} 次错误请求，可到排行的“错误”页签查看。
        </Callout>
      )}

      <Tabs value={activeTab} onValueChange={(value) => setOverviewTab(value as OverviewTab)}>
        <TabsList className="w-full justify-start overflow-x-auto">
          <TabsTrigger value="summary">汇总</TabsTrigger>
          <TabsTrigger value="accounts">账号</TabsTrigger>
          <TabsTrigger value="rankings">排行</TabsTrigger>
        </TabsList>

        <TabsContent value="summary">
          <div className="space-y-3">
            <OverallSummarySection
              metrics={summaryData.totals.all}
              billing={summaryData.external}
              accountCostUsd={overviewData?.accountCostUsd ?? 0}
              rawTokens={overviewData?.rawTokens ?? 0}
              realtime={usageSummary.data?.realtime}
            />
            <OverviewTrendCard
              title="分时趋势"
              icon={<TrendingUp />}
              views={summaryTrendViews()}
              points={seriesPoints}
              hourly={seriesGranularity === 'hour'}
              partialFrom={partialFrom}
              loading={series.isFetching}
              onDrillDown={seriesGranularity === 'day' ? drillDown : undefined}
            />
          </div>
        </TabsContent>

        <TabsContent value="accounts">
          <div className="space-y-3">
            <OverviewTrendCard
              title="账号分时趋势"
              icon={<DollarSign />}
              views={accountTrendViews(seriesGroups)}
              points={seriesPoints}
              hourly={seriesGranularity === 'hour'}
              partialFrom={partialFrom}
              loading={series.isFetching}
              onDrillDown={seriesGranularity === 'day' ? drillDown : undefined}
            />
            {(overview.isLoading && !overviewData) || (accounts.isLoading && !accounts.data) ? (
              <LoadingState text="加载账号用量..." />
            ) : (
              <AccountsSection
                billing={billing}
                accounts={accounts.data?.accounts ?? []}
                statuses={accountStatuses.data?.accounts ?? []}
                range={summaryData.range}
                navigate={navigate}
              />
            )}
          </div>
        </TabsContent>

        <TabsContent value="rankings">
          {rankings.isLoading && !rankings.data ? (
            <LoadingState text="加载排行..." />
          ) : rankings.data ? (
            <RankingsSection rankings={rankings.data} />
          ) : (
            <EmptyState title="暂无排行数据" />
          )}
        </TabsContent>
      </Tabs>

      <p className="text-xs text-muted-foreground">数据更新：{formatDate(summaryData.generatedAt)}</p>
    </PageContainer>
  )
}
