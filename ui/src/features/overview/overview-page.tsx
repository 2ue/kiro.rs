import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  DollarSign,
  Edit3,
  Layers3,
  RefreshCw,
  Route as RouteIcon,
  TrendingUp,
  Wallet,
  Zap,
} from 'lucide-react'
import { getAccounts, getAccountsStatus } from '@/api/accounts'
import { getRoutes } from '@/api/routes'
import { CACHE_STRATEGIES } from '@/features/cache-policies/route-cache-editor'
import { routeEditorPath } from '@/features/routes/route-utils'
import {
  useUsageAccountLatency,
  useUsageOverview,
  useUsageOverviewRankings,
  useUsageOverviewSeries,
  useUsageOverviewSummary,
  useUsageRouteLatency,
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
import { OverviewTrendCard, accountTrendViews, routeTrendViews, summaryTrendViews } from './overview-trends'
import {
  buildRouteOverviewRows,
  routeInsights,
  routeLabelResolver,
  type RouteOverviewRow,
  type RouteRowStatus,
} from './route-overview'
import { billingDeltaTextClass, billingDeltaTone } from '@/features/usage/usage-helpers'
import { RequestApiKeyIdDisplay } from '@/features/usage/request-api-key-id'
import { normalizeRequestApiKeyId } from '@/lib/request-api-key-id'
import { formatCompact, formatDate, formatNumber, formatPercent, formatUsdFixed2 } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import type {
  Account,
  AccountStatus,
  UsageAccountLatencyRow,
  UsageOverviewMetrics,
  UsageOverviewPoolRow,
  UsageOverviewRankRow,
  UsageOverviewRouteRow,
  UsageOverviewSeriesPoint,
  UsageRouteLatencyRow,
  RouteConfig,
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
type OverviewTab = 'summary' | 'accounts' | 'routes' | 'rankings'
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

/** 跳到用量页，带上账号 / 路由 / 模型 / 请求 Key 与时间范围筛选。 */
function usageLink(
  navigate: ReturnType<typeof useNavigate>,
  params: {
    accountId?: number
    endpoint?: string
    /** 路由前缀，默认路由为 "/" */
    route?: string
    model?: string
    requestApiKeyId?: string
    since: string
    until: string
  },
) {
  const search = new URLSearchParams()
  if (params.accountId) search.set('accountId', String(params.accountId))
  if (params.endpoint) search.set('endpoint', params.endpoint)
  if (params.route) search.set('route', params.route)
  if (params.model) search.set('model', params.model)
  if (params.requestApiKeyId) search.set('requestApiKeyId', params.requestApiKeyId)
  if (params.since) search.set('since', localDateTimeValue(params.since))
  if (params.until) search.set('until', localDateTimeValue(params.until))
  navigate(`/usage?${search.toString()}`)
}

/** 排行各页签点击一行时带到用量页的筛选键。错误页签没有对应筛选。 */
function rankUsageFilter(dimension: RankingDimension, key: string): Parameters<typeof usageLink>[1] | null {
  const base = { since: '', until: '' }
  switch (dimension) {
    case 'models':
      return { ...base, model: key }
    case 'paths':
      return { ...base, endpoint: key }
    case 'keys':
      return { ...base, requestApiKeyId: key }
    case 'accounts': {
      const accountId = Number(key)
      return Number.isFinite(accountId) && accountId > 0 ? { ...base, accountId } : null
    }
    default:
      return null
  }
}

function formatMs(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '-'
  return `${formatNumber(Math.round(value))} ms`
}

/** 延迟分位扫明细，后端限制范围 8 天以内；更长范围不请求、列显示 -。 */
const LATENCY_MAX_RANGE_MS = 8 * 86_400_000

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
  latency,
  latencyUnavailable,
  range,
  navigate,
}: {
  billing: BillingTotals & { pools: UsageOverviewPoolRow[] }
  accounts: Account[]
  statuses: AccountStatus[]
  /** 按账号的延迟分位；范围超过 8 天或接口失败时为空 */
  latency: UsageAccountLatencyRow[]
  latencyUnavailable: string
  range: { from: string; to: string }
  navigate: ReturnType<typeof useNavigate>
}) {
  const usageById = new Map(billing.pools.map((item) => [item.poolId, item]))
  const latencyById = new Map(latency.map((item) => [item.accountId, item]))
  const totalAccountCostUsd = billing.pools.reduce((sum, item) => sum + (item.accountCostUsd ?? 0), 0)
  const totalAttempts = billing.pools.reduce((sum, item) => sum + (item.attemptRequests ?? 0), 0)
  const totalAttemptErrors = billing.pools.reduce((sum, item) => sum + (item.attemptErrorRequests ?? 0), 0)
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
        <StatCard
          title="上游原始成本"
          value={formatUsdFixed2(billing.rawCostUsd)}
          desc={`账号成本 ${formatUsdFixed2(totalAccountCostUsd)}（× 各账号倍率）`}
          icon={<DollarSign />}
          tone="warning"
        />
        <StatCard
          title="展示计费"
          value={formatUsdFixed2(billing.shapedCostUsd)}
          desc="整形后的中间成本口径"
          icon={<DollarSign />}
          tone="primary"
        />
        <StatCard
          title="账号尝试"
          value={formatNumber(totalAttempts)}
          desc={totalAttempts > 0
            ? `含切走 ${formatNumber(totalAttemptErrors)} 次失败 · 尝试错误率 ${formatPercent(metricsShare(totalAttemptErrors, totalAttempts))}`
            : '每次落到账号的调度都算一次，含中途失败被切走的'}
          icon={<Activity />}
          tone={totalAttempts > 0 && totalAttemptErrors / totalAttempts > 0.05 ? 'warning' : 'info'}
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
        {' '}是最终上报口径，可计费取最终可计费金额；
        <span className="ml-1 font-medium text-foreground/75" title="上游原始成本 × 账号成本倍率">账号成本</span>
        {' '}按账号倍率折算。
        <span className="ml-1 font-medium text-foreground/75">错误率</span>
        {' '}先看最终落到该账号的请求，再看“含切走”：把中途失败后换到别的账号的尝试也算进来。
        {latencyUnavailable && <span className="ml-1">耗时分位：{latencyUnavailable}</span>}
      </div>
      {rows.length === 0 ? (
        <EmptyState title="暂无账号" className="mt-3 py-8" />
      ) : (
        <div className="mt-4 scrollbar-thin overflow-x-auto">
          <Table className="min-w-[1500px] text-xs">
            <TableHeader>
              <TableRow>
                <TableHead>账号</TableHead>
                <TableHead className="text-right">请求</TableHead>
                <TableHead className="text-right" title="最终：落到该账号并由它返回的请求的错误率；含切走：把中途失败后被切到别的账号的尝试也算进来">错误率</TableHead>
                <TableHead className="text-right">上游原始成本</TableHead>
                <TableHead className="text-right" title="上游原始成本 × 账号成本倍率">账号成本</TableHead>
                <TableHead className="text-right" title="按整形后的展示 usage 计算的中间成本">展示计费</TableHead>
                <TableHead className="text-right" title="按最终上报 usage 计算，包含输出补偿和成本底线修复">补偿后计费</TableHead>
                <TableHead className="text-right">差额占原始</TableHead>
                <TableHead className="text-right">原始/整形倍率</TableHead>
                <TableHead className="text-right" title="缓存读 ÷（未缓存输入 + 缓存读 + 缓存写）">缓存命中率</TableHead>
                <TableHead className="text-right" title="该账号请求总耗时的中位数 / 95 分位，直接按明细算">p50 / p95 耗时</TableHead>
                <TableHead className="text-right" title="流式请求首字延迟的 95 分位">p95 首字</TableHead>
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
                const attemptRequests = usage?.attemptRequests ?? 0
                const attemptErrorRate = usage?.attemptErrorRate ?? 0
                const rowLatency = latencyById.get(accountId)
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
                    <TableCell
                      className="text-right font-mono"
                      title={attemptRequests > 0
                        ? `最终落到该账号的 ${formatNumber(metrics.requests)} 次请求里错误 ${formatNumber(metrics.errorRequests)} 次；算上中途失败被切走的，共尝试 ${formatNumber(attemptRequests)} 次、失败 ${formatNumber(usage?.attemptErrorRequests ?? 0)} 次`
                        : `最终落到该账号的 ${formatNumber(metrics.requests)} 次请求里错误 ${formatNumber(metrics.errorRequests)} 次`}
                    >
                      <span className={cn(metrics.errorRate > 0.05 && 'text-destructive')}>最终 {formatPercent(metrics.errorRate)}</span>
                      {attemptRequests > 0 && (
                        <span className={cn('ml-1 text-muted-foreground', attemptErrorRate > 0.05 && 'text-warning')}>· 含切走 {formatPercent(attemptErrorRate)}</span>
                      )}
                    </TableCell>
                    <TableCell className="text-right font-mono">{formatUsdFixed2(rawCostUsd)}</TableCell>
                    <TableCell className="text-right font-mono" title="上游原始成本 × 账号成本倍率">{formatUsdFixed2(usage?.accountCostUsd ?? 0)}</TableCell>
                    <TableCell className="text-right font-mono" title="整形后的中间成本口径">{formatUsdFixed2(shapedCostUsd)}</TableCell>
                    <TableCell className="text-right font-mono" title="最终上报/补偿后成本口径；当前用于可计费金额">{formatUsdFixed2(upliftedCostUsd)}</TableCell>
                    <TableCell className={cn('text-right font-mono', billingDeltaTextClass(rowTone))}>
                      <div>{rowDelta >= 0 ? '+' : ''}{formatUsdFixed2(rowDelta)}</div>
                      <div className="text-[0.62rem]" title="当前账号差额 ÷ 当前账号上游原始成本">{costDeltaPercent(rowDelta, rawCostUsd)}</div>
                    </TableCell>
                    <TableCell className="text-right font-mono" title="上游原始成本 ÷ 整形后展示成本">{costMultiplier(rawCostUsd, shapedCostUsd)}</TableCell>
                    <TableCell className="text-right"><HitRateCell metrics={metrics} /></TableCell>
                    <TableCell
                      className="text-right font-mono"
                      title={rowLatency ? `${formatNumber(rowLatency.samples)} 条请求 · p99 ${formatMs(rowLatency.p99DurationMs)}` : undefined}
                    >
                      {rowLatency ? `${formatMs(rowLatency.p50DurationMs)} / ${formatMs(rowLatency.p95DurationMs)}` : '-'}
                    </TableCell>
                    <TableCell
                      className="text-right font-mono"
                      title={rowLatency && rowLatency.firstTokenSamples > 0 ? `${formatNumber(rowLatency.firstTokenSamples)} 条流式请求 · p50 ${formatMs(rowLatency.p50FirstTokenLatencyMs)}` : undefined}
                    >
                      {rowLatency && rowLatency.firstTokenSamples > 0 ? formatMs(rowLatency.p95FirstTokenLatencyMs) : '-'}
                    </TableCell>
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

// ─── 路由分区 ───────────────────────────────────────────────────────────────────

const ROUTE_STATUS_BADGE: Record<RouteRowStatus, { label: string; tone: 'success' | 'neutral' | 'warning' }> = {
  enabled: { label: '启用', tone: 'success' },
  disabled: { label: '停用', tone: 'neutral' },
  deleted: { label: '已删除', tone: 'neutral' },
  unknown: { label: '未识别', tone: 'warning' },
}

/** 缓存策略名称沿用路由编辑页的叫法；未设置时按后端默认的本地模拟缓存。 */
function routeStrategyLabel(route?: RouteConfig): string | undefined {
  if (!route) return undefined
  const type = route.cachePolicy?.cacheType ?? 'current_high_cache'
  return CACHE_STRATEGIES.find(([key]) => key === type)?.[1] ?? type
}

function RoutesSection({
  routes,
  usage,
  latency,
  latencyUnavailable,
  range,
  navigate,
}: {
  routes: RouteConfig[]
  usage: UsageOverviewRouteRow[]
  /** 按路由的延迟分位；范围超过 8 天或接口失败时为空 */
  latency: UsageRouteLatencyRow[]
  latencyUnavailable: string
  range: { from: string; to: string }
  navigate: ReturnType<typeof useNavigate>
}) {
  const rows = buildRouteOverviewRows(routes, usage, latency)
  const insights = routeInsights(rows)
  const labelOf = routeLabelResolver(routes)
  const totalBillableUsd = rows.reduce((sum, row) => sum + (row.usage?.billableCostUsd ?? 0), 0)
  const rowLabel = (row: RouteOverviewRow) => labelOf(row.key)
  return (
    <SectionCard>
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard
          title="活跃路由"
          value={formatNumber(insights.activeRoutes)}
          desc={`已启用 ${formatNumber(insights.enabledRoutes)} / 已配置 ${formatNumber(insights.configuredRoutes)}${insights.unconfiguredActiveRoutes > 0 ? ` · ${formatNumber(insights.unconfiguredActiveRoutes)} 个已删除前缀仍有流量` : ''}`}
          icon={<RouteIcon />}
          tone="info"
        />
        <StatCard
          title="可计费最高"
          value={insights.topBillable ? formatUsdFixed2(insights.topBillable.usage?.billableCostUsd ?? 0) : '-'}
          desc={insights.topBillable
            ? `${rowLabel(insights.topBillable)} · 占 ${formatPercent(metricsShare(insights.topBillable.usage?.billableCostUsd ?? 0, totalBillableUsd))}`
            : '当前范围没有可计费金额'}
          icon={<DollarSign />}
          tone="primary"
        />
        <StatCard
          title="命中率最低"
          value={insights.lowestHitRate ? formatHitRate(insights.lowestHitRate.rate) : '-'}
          desc={insights.lowestHitRate
            ? [rowLabel(insights.lowestHitRate.row), routeStrategyLabel(insights.lowestHitRate.row.route)].filter(Boolean).join(' · ')
            : '当前范围没有输入 token'}
          icon={<Layers3 />}
          tone={insights.lowestHitRate ? HIT_RATE_TONE[cacheHitRateLevel(insights.lowestHitRate.rate)] : 'default'}
        />
        <StatCard
          title="错误率最高"
          value={insights.highestErrorRate ? formatPercent(insights.highestErrorRate.rate) : '无错误'}
          desc={insights.highestErrorRate
            ? `${rowLabel(insights.highestErrorRate.row)} · ${formatNumber(insights.highestErrorRate.row.usage?.metrics.errorRequests ?? 0)} / ${formatNumber(insights.highestErrorRate.row.usage?.metrics.requests ?? 0)} 次`
            : '有流量的路由都没有错误请求'}
          icon={<AlertTriangle />}
          tone={!insights.highestErrorRate ? 'success' : insights.highestErrorRate.rate > 0.05 ? 'error' : 'warning'}
        />
      </div>
      <div className="mt-3 rounded-lg bg-muted/25 px-3 py-2 text-xs text-muted-foreground">
        按请求进入的路由前缀汇总，前缀之后的入口路径（messages、count_tokens 等）合并计算；
        <span className="ml-1 font-medium text-foreground/75">展示计费</span>
        {' '}是整形后的中间口径，
        <span className="ml-1 font-medium text-foreground/75">补偿后计费</span>
        {' '}是最终上报口径，
        <span className="ml-1 font-medium text-foreground/75">可计费</span>
        {' '}取最终可计费金额，
        <span className="ml-1 font-medium text-foreground/75">账号成本</span>
        {' '}按各账号倍率折算。命中率随路由的缓存策略变化，对照策略名称看。
        {latencyUnavailable && <span className="ml-1">耗时分位：{latencyUnavailable}</span>}
      </div>
      {rows.length === 0 ? (
        <EmptyState title="暂无路由" className="mt-3 py-8" />
      ) : (
        <div className="mt-4 scrollbar-thin overflow-x-auto">
          <Table className="min-w-[1500px] text-xs">
            <TableHeader>
              <TableRow>
                <TableHead>路由</TableHead>
                <TableHead className="text-right">请求</TableHead>
                <TableHead className="text-right" title="错误请求 ÷ 请求数">错误率</TableHead>
                <TableHead className="text-right">上游原始成本</TableHead>
                <TableHead className="text-right" title="上游原始成本 × 账号成本倍率">账号成本</TableHead>
                <TableHead className="text-right" title="按整形后的展示 usage 计算的中间成本">展示计费</TableHead>
                <TableHead className="text-right" title="按最终上报 usage 计算，包含输出补偿和成本底线修复">补偿后计费</TableHead>
                <TableHead className="text-right" title="最终对下游的可计费金额">可计费</TableHead>
                <TableHead className="text-right">差额占原始</TableHead>
                <TableHead className="text-right">原始/整形倍率</TableHead>
                <TableHead className="text-right" title="缓存读 ÷（未缓存输入 + 缓存读 + 缓存写）">缓存命中率</TableHead>
                <TableHead className="text-right" title="该路由请求总耗时的中位数 / 95 分位，直接按明细算">p50 / p95 耗时</TableHead>
                <TableHead className="text-right" title="流式请求首字延迟的 95 分位">p95 首字</TableHead>
                <TableHead className="text-right">未计价</TableHead>
                <TableHead className="text-right">兜底</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((row) => {
                const { key, route, name, status, usage: rowUsage, latency: rowLatency } = row
                const metrics = rowUsage?.metrics ?? EMPTY_OVERVIEW_METRICS
                const rawCostUsd = rowUsage?.rawCostUsd ?? 0
                const shapedCostUsd = rowUsage?.shapedCostUsd ?? 0
                const upliftedCostUsd = rowUsage?.upliftedCostUsd ?? 0
                const rowDelta = rowUsage?.profitUsd ?? (upliftedCostUsd - rawCostUsd)
                const rowTone = billingDeltaTone(rowDelta)
                const badge = ROUTE_STATUS_BADGE[status]
                const strategy = routeStrategyLabel(route)
                const endpoints = rowUsage?.endpoints ?? []
                return (
                  <TableRow key={key}>
                    <TableCell>
                      <div className="flex min-w-[220px] flex-wrap items-center gap-1.5">
                        {key === 'unknown' ? (
                          <span className="text-xs font-semibold">{name}</span>
                        ) : (
                          <Button
                            variant="link"
                            size="xs"
                            className="h-auto max-w-[220px] justify-start truncate px-0 text-left text-xs font-semibold"
                            title={`路由 ${key} · 查看用量明细`}
                            onClick={() => usageLink(navigate, { route: key, since: range.from, until: range.to })}
                          >
                            {name}
                          </Button>
                        )}
                        <Badge tone={badge.tone} size="xs">{badge.label}</Badge>
                        {metrics.requests === 0 && <Badge tone="secondary" size="xs">无流量</Badge>}
                        {route && (
                          <Button
                            variant="ghost"
                            size="icon-xs"
                            title="编辑路由"
                            aria-label={`编辑路由 ${key}`}
                            onClick={() => navigate(routeEditorPath(route.id))}
                          >
                            <Edit3 className="size-3" />
                          </Button>
                        )}
                      </div>
                      <div
                        className="mt-0.5 font-mono text-[0.62rem] text-muted-foreground"
                        title={endpoints.length > 0 ? `入口：${endpoints.join('、')}` : undefined}
                      >
                        {key}{strategy && <span className="font-sans"> · {strategy}</span>}
                      </div>
                    </TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(metrics.requests)}>{formatNumber(metrics.requests)}</TableCell>
                    <TableCell
                      className={cn('text-right font-mono', metrics.errorRate > 0.05 && 'text-destructive')}
                      title={`${formatNumber(metrics.requests)} 次请求里错误 ${formatNumber(metrics.errorRequests)} 次`}
                    >
                      {metrics.requests > 0 ? formatPercent(metrics.errorRate) : '-'}
                    </TableCell>
                    <TableCell className="text-right font-mono">{formatUsdFixed2(rawCostUsd)}</TableCell>
                    <TableCell className="text-right font-mono" title="上游原始成本 × 账号成本倍率">{formatUsdFixed2(rowUsage?.accountCostUsd ?? 0)}</TableCell>
                    <TableCell className="text-right font-mono" title="整形后的中间成本口径">{formatUsdFixed2(shapedCostUsd)}</TableCell>
                    <TableCell className="text-right font-mono" title="最终上报/补偿后成本口径">{formatUsdFixed2(upliftedCostUsd)}</TableCell>
                    <TableCell className="text-right font-mono" title="最终对下游的可计费金额">{formatUsdFixed2(rowUsage?.billableCostUsd ?? 0)}</TableCell>
                    <TableCell className={cn('text-right font-mono', billingDeltaTextClass(rowTone))}>
                      <div>{rowDelta >= 0 ? '+' : ''}{formatUsdFixed2(rowDelta)}</div>
                      <div className="text-[0.62rem]" title="当前路由差额 ÷ 当前路由上游原始成本">{costDeltaPercent(rowDelta, rawCostUsd)}</div>
                    </TableCell>
                    <TableCell className="text-right font-mono" title="上游原始成本 ÷ 整形后展示成本">{costMultiplier(rawCostUsd, shapedCostUsd)}</TableCell>
                    <TableCell className="text-right"><HitRateCell metrics={metrics} /></TableCell>
                    <TableCell
                      className="text-right font-mono"
                      title={rowLatency ? `${formatNumber(rowLatency.samples)} 条请求 · p99 ${formatMs(rowLatency.p99DurationMs)}` : undefined}
                    >
                      {rowLatency ? `${formatMs(rowLatency.p50DurationMs)} / ${formatMs(rowLatency.p95DurationMs)}` : '-'}
                    </TableCell>
                    <TableCell
                      className="text-right font-mono"
                      title={rowLatency && rowLatency.firstTokenSamples > 0 ? `${formatNumber(rowLatency.firstTokenSamples)} 条流式请求 · p50 ${formatMs(rowLatency.p50FirstTokenLatencyMs)}` : undefined}
                    >
                      {rowLatency && rowLatency.firstTokenSamples > 0 ? formatMs(rowLatency.p95FirstTokenLatencyMs) : '-'}
                    </TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(metrics.unpricedRequests)}>{formatNumber(metrics.unpricedRequests)}</TableCell>
                    <TableCell className="text-right font-mono" title={formatNumber(rowUsage?.costFloorAppliedRequests ?? 0)}>{formatNumber(rowUsage?.costFloorAppliedRequests ?? 0)}</TableCell>
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

/** 排行行的缓存命中率：后端给分子分母，这里按同一口径算。 */
function rankHitRate(row: UsageOverviewRankRow): number | null {
  return cacheHitRate({
    uncachedInputTokens: row.uncachedInputTokens ?? 0,
    cacheReadInputTokens: row.cacheReadInputTokens ?? 0,
    cacheCreationInputTokens: row.cacheCreationInputTokens ?? 0,
  })
}

function RankPanel({
  dimension,
  rows,
  total,
  errorRank = false,
  onOpen,
}: {
  dimension: RankingDimension
  rows: UsageOverviewRankRow[]
  total: number
  errorRank?: boolean
  /** 点击一行跳到用量页；错误页签没有筛选键，不可点 */
  onOpen?: (row: UsageOverviewRankRow) => void
}) {
  const isKeys = dimension === 'keys'
  return (
    <div className="mt-3">
      {rows.length === 0 ? (
        <EmptyState title="暂无排行数据" className="py-8" />
      ) : (
        <Table className={errorRank ? 'min-w-[560px]' : 'min-w-[1140px]'}>
          <TableHeader>
            <TableRow>
              <TableHead className="w-10">#</TableHead>
              <TableHead>名称</TableHead>
              <TableHead className="text-right">{errorRank ? '错误数' : '请求数'}</TableHead>
              <TableHead className="text-right">{errorRank ? '占错误' : '占请求'}</TableHead>
              {!errorRank && <TableHead className="text-right" title="错误请求 ÷ 请求数">错误率</TableHead>}
              {!errorRank && <TableHead className="text-right" title="缓存读 ÷（未缓存输入 + 缓存读 + 缓存写）">缓存命中率</TableHead>}
              <TableHead className="text-right" title="按最终上报 usage 计价">估算计费</TableHead>
              {!errorRank && <TableHead className="text-right" title="按上游原始 usage 计价">原始计费</TableHead>}
              {!errorRank && <TableHead className="text-right" title="上游原始成本 × 各账号成本倍率">账号成本</TableHead>}
              {!errorRank && <TableHead className="text-right" title="估算计费 - 账号成本，括号里是占账号成本的比例">差额</TableHead>}
              {!errorRank && <TableHead className="text-right">平均耗时</TableHead>}
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, index) => {
              const errorRate = metricsShare(row.errorRequests, row.requests)
              const hitRate = rankHitRate(row)
              const hitLevel = cacheHitRateLevel(hitRate)
              const accountCostUsd = row.accountCostUsd ?? 0
              const delta = accountCostDelta(row.estimatedCostUsd, accountCostUsd)
              const clickable = Boolean(onOpen)
              return (
                <TableRow
                  key={row.key}
                  className={cn(clickable && 'cursor-pointer')}
                  title={clickable ? '查看该项的用量明细' : undefined}
                  onClick={clickable ? () => onOpen?.(row) : undefined}
                >
                  <TableCell className="font-mono text-xs text-muted-foreground">{index + 1}</TableCell>
                  <TableCell>
                    <div className="max-w-[260px] truncate text-xs font-semibold" title={row.label}>{row.label}</div>
                    {isKeys && normalizeRequestApiKeyId(row.key) ? (
                      <div className="mt-0.5 text-muted-foreground/70" onClick={(event) => event.stopPropagation()}>
                        <RequestApiKeyIdDisplay value={row.key} />
                      </div>
                    ) : (
                      row.label !== row.key && <div className="max-w-[260px] truncate font-mono text-[0.62rem] text-muted-foreground/60">{row.key}</div>
                    )}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs" title={formatNumber(errorRank ? row.errorRequests : row.requests)}>
                    {formatCompact(errorRank ? row.errorRequests : row.requests)}
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs">{formatPercent(metricsShare(errorRank ? row.errorRequests : row.requests, total))}</TableCell>
                  {!errorRank && (
                    <TableCell className={cn('text-right font-mono text-xs', errorRate > 0.05 && 'text-destructive')} title={`${formatNumber(row.errorRequests)} 次错误`}>
                      {formatPercent(errorRate)}
                    </TableCell>
                  )}
                  {!errorRank && (
                    <TableCell className={cn('text-right font-mono text-xs', HIT_RATE_TEXT[hitLevel])} title={HIT_RATE_HINT[hitLevel] || undefined}>
                      {formatHitRate(hitRate)}
                    </TableCell>
                  )}
                  <TableCell className="text-right font-mono text-xs">{formatUsdFixed2(row.estimatedCostUsd)}</TableCell>
                  {!errorRank && <TableCell className="text-right font-mono text-xs">{formatUsdFixed2(row.originalCostUsd ?? 0)}</TableCell>}
                  {!errorRank && <TableCell className="text-right font-mono text-xs">{formatUsdFixed2(accountCostUsd)}</TableCell>}
                  {!errorRank && (
                    <TableCell
                      className={cn('text-right font-mono text-xs', delta.tone === 'loss' && 'text-destructive')}
                      title={`占账号成本 ${delta.percentText}`}
                    >
                      {`${delta.deltaUsd >= 0 ? '+' : '-'}${formatUsdFixed2(Math.abs(delta.deltaUsd))}`}
                      <div className="text-[0.62rem] text-muted-foreground">{delta.percentText}</div>
                    </TableCell>
                  )}
                  {!errorRank && <TableCell className="text-right font-mono text-xs">{row.requests > 0 ? formatMs(row.averageDurationMs) : '-'}</TableCell>}
                </TableRow>
              )
            })}
          </TableBody>
        </Table>
      )}
    </div>
  )
}

function RankingsSection({
  rankings,
  range,
  navigate,
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
  range: { from: string; to: string }
  navigate: ReturnType<typeof useNavigate>
}) {
  const [dimension, setDimension] = useState<RankingDimension>('models')
  const openUsage = (view: RankingDimension) => (row: UsageOverviewRankRow) => {
    const filter = rankUsageFilter(view, row.key)
    if (!filter) return
    usageLink(navigate, { ...filter, since: range.from, until: range.to })
  }
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
            <RankPanel
              dimension={view.key}
              rows={view.rows}
              total={view.total}
              errorRank={view.errorRank}
              onOpen={view.errorRank ? undefined : openUsage(view.key)}
            />
          </TabsContent>
        ))}
        <p className="mt-2 text-[0.68rem] text-muted-foreground">点击一行跳到用量页查看该项在当前范围内的明细；错误页签按错误类型聚合，没有对应筛选。</p>
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
  const activeTab: OverviewTab = tabParam === 'accounts' || tabParam === 'routes' || tabParam === 'rankings' ? tabParam : 'summary'

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
  const series = useUsageOverviewSeries(queryParams, refreshInterval, (activeTab === 'summary' || activeTab === 'accounts' || activeTab === 'routes') && !summary.isError)
  const rankings = useUsageOverviewRankings(rankingQueryParams, refreshInterval, activeTab === 'rankings' && !summary.isError)
  // 延迟分位扫明细，后端限制 8 天以内：30 天 / 本月 / 过长的自定义范围不请求。
  const latencyRangeOk = rangeKey
    ? ['today', 'yesterday', 'last24h', 'last7d'].includes(rangeKey)
    : !fromParam || !toParam || Date.parse(toParam) - Date.parse(fromParam) <= LATENCY_MAX_RANGE_MS
  const latencyParams = useMemo(() => {
    const { granularity: _granularity, ...rest } = queryParams as typeof queryParams & { granularity?: string }
    return rest
  }, [queryParams])
  const latency = useUsageAccountLatency(latencyParams, refreshInterval, activeTab === 'accounts' && latencyRangeOk && !summary.isError)
  const routeLatency = useUsageRouteLatency(latencyParams, refreshInterval, activeTab === 'routes' && latencyRangeOk && !summary.isError)
  // 路由配置给路由表补上无流量的路由、显示名、启停状态和缓存策略。
  const routeConfigs = useQuery({ queryKey: ['routes'], queryFn: getRoutes, enabled: activeTab === 'routes', staleTime: 30_000 })
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
    latency.error ? `账号耗时分位：${extractErrorMessage(latency.error)}` : '',
    routeLatency.error ? `路由耗时分位：${extractErrorMessage(routeLatency.error)}` : '',
    routeConfigs.error ? `路由配置：${extractErrorMessage(routeConfigs.error)}` : '',
  ].filter(Boolean)
  const latencyUnavailable = !latencyRangeOk
    ? '只在 8 天以内的范围计算（需要扫用量明细）'
    : latency.error
      ? '读取失败'
      : ''
  const routeLatencyUnavailable = !latencyRangeOk
    ? '只在 8 天以内的范围计算（需要扫用量明细）'
    : routeLatency.error
      ? '读取失败'
      : ''

  const refetchAll = () => {
    void summary.refetch()
    void overview.refetch()
    void series.refetch()
    void usageSummary.refetch()
    if (activeTab === 'accounts') {
      void accounts.refetch()
      void accountStatuses.refetch()
      if (latencyRangeOk) void latency.refetch()
    }
    if (activeTab === 'routes') {
      void routeConfigs.refetch()
      if (latencyRangeOk) void routeLatency.refetch()
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
          <TabsTrigger value="routes">路由</TabsTrigger>
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
              views={summaryTrendViews({ hourly: seriesGranularity === 'hour' })}
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
                latency={latencyRangeOk ? latency.data?.accounts ?? [] : []}
                latencyUnavailable={latencyUnavailable}
                range={summaryData.range}
                navigate={navigate}
              />
            )}
          </div>
        </TabsContent>

        <TabsContent value="routes">
          <div className="space-y-3">
            <OverviewTrendCard
              title="路由分时趋势"
              icon={<RouteIcon />}
              views={routeTrendViews(seriesGroups, routeLabelResolver(routeConfigs.data ?? []))}
              points={seriesPoints}
              hourly={seriesGranularity === 'hour'}
              partialFrom={partialFrom}
              loading={series.isFetching}
              onDrillDown={seriesGranularity === 'day' ? drillDown : undefined}
            />
            {(overview.isLoading && !overviewData) || (routeConfigs.isLoading && !routeConfigs.data) ? (
              <LoadingState text="加载路由用量..." />
            ) : (
              <RoutesSection
                routes={routeConfigs.data ?? []}
                usage={overviewData?.routes ?? []}
                latency={latencyRangeOk ? routeLatency.data?.routes ?? [] : []}
                latencyUnavailable={routeLatencyUnavailable}
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
            <RankingsSection rankings={rankings.data} range={summaryData.range} navigate={navigate} />
          ) : (
            <EmptyState title="暂无排行数据" />
          )}
        </TabsContent>
      </Tabs>

      <p className="text-xs text-muted-foreground">数据更新：{formatDate(summaryData.generatedAt)}</p>
    </PageContainer>
  )
}
