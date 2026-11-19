import { useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  Activity,
  AlertTriangle,
  BarChart3,
  Boxes,
  Copy,
  DollarSign,
  ExternalLink,
  FileClock,
  Filter,
  Gauge,
  RefreshCw,
  Search,
} from 'lucide-react'
import { toast } from 'sonner'
import { useAccountEventWindowSummary, useAuditLogsPage, useUsageDashboardAccountRisk } from '@/hooks/use-usage'
import { useAccountQualityHistory } from '@/hooks/use-accounts'
import { ACTOR_LABELS, accountEventView, summarizeAccountEvents, summaryHonorsWindow, summaryTiles } from '@/features/audit/account-events'
import {
  bucketDescription,
  formatMsTick,
  formatQualityLabel,
  formatQualityTick,
  formatRateTick,
  pointsInWindow,
  qualityChartRows,
  rowsCrossDays,
} from './quality-history'
import { riskInitialFromSearch } from './risk-url-params'
import { TrendAreaChart } from '@/components/charts'
import { formatCompact, formatDate, formatNumber, formatPercent, formatUsd } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import type {
  UsageAccountRiskBucket,
  UsageAccountRiskCacheStats,
  UsageAccountRiskGroup,
  UsageAccountRiskQuery,
  UsageAccountRiskSample,
} from '@/types/api'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  EmptyState,
  LoadingState,
  ErrorState,
  Callout,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'

const TIMEZONE = 'Asia/Shanghai'
const DEFAULT_WARNING_TOKENS = 800_000
const DEFAULT_CRITICAL_TOKENS = 1_000_000

type StreamFilter = 'all' | 'stream' | 'non_stream'

function tokenTitle(value: number): string {
  return `${formatNumber(value)} tokens`
}

function signedUsd(value: number): string {
  const text = formatUsd(value)
  return value > 0 ? `+${text}` : text
}

function costRatio(value?: number | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return formatPercent(value as number)
}

function parsePositiveInt(value: string, fallback: number): number {
  const parsed = Number.parseInt(value.replace(/[,_\s]/g, ''), 10)
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback
}

function toDatetimeLocalValue(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, '0')
  return [
    date.getFullYear(),
    '-',
    pad(date.getMonth() + 1),
    '-',
    pad(date.getDate()),
    'T',
    pad(date.getHours()),
    ':',
    pad(date.getMinutes()),
  ].join('')
}

function recentDatetimeLocal(hours: number): string {
  return toDatetimeLocalValue(new Date(Date.now() - hours * 60 * 60 * 1000))
}

function datetimeLocalToIso(value: string): string | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const date = new Date(trimmed)
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString()
}

function streamValueToQuery(value: StreamFilter): boolean | undefined {
  if (value === 'stream') return true
  if (value === 'non_stream') return false
  return undefined
}

function poolLabel(id?: number, name?: string): string {
  if (!id && !name) return '未知账号'
  return [`#${id ?? '-'}`, name].filter(Boolean).join(' ')
}

function riskReasonLabel(reason: string): string {
  const labels: Record<string, string> = {
    missing_account_billing: '缺少计费记录',
    output_zero: '输出为 0',
    raw_cache_critical: '上游缓存超高',
    reported_cache_critical: '最终缓存超高',
    raw_cache_warning: '上游缓存偏高',
    reported_cache_warning: '最终缓存偏高',
    below_raw_cost: '低于上游成本',
    below_target_cost: '低于目标成本',
  }
  return labels[reason] ?? reason
}

function toneForRisk(criticalCount: number, warningCount: number): 'default' | 'success' | 'warning' | 'error' {
  if (criticalCount > 0) return 'error'
  if (warningCount > 0) return 'warning'
  return 'success'
}

/** 用量页的请求明细链接；用量页已支持 requestId 参数。 */
function usageRecordLink(requestId: string): string {
  return `/usage?${new URLSearchParams({ requestId }).toString()}`
}

async function copyRequestId(id: string) {
  try {
    await navigator.clipboard.writeText(id)
    toast.success('请求 ID 已复制')
  } catch (error) {
    toast.error(`复制失败: ${extractErrorMessage(error)}`)
  }
}

function formatMs(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '-'
  return `${formatNumber(Math.round(value))} ms`
}

export function AccountRiskPage() {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  // 账号卡片“诊断”跳过来时带 accountId（也可带时间窗口）；只在进入页面时读一次作为初值。
  const [initial] = useState(() => riskInitialFromSearch(searchParams))
  const [windowKey, setWindowKey] = useState<string>(initial.windowKey)
  const [warningInput, setWarningInput] = useState(String(DEFAULT_WARNING_TOKENS))
  const [criticalInput, setCriticalInput] = useState(String(DEFAULT_CRITICAL_TOKENS))
  const [poolInput, setPoolInput] = useState(initial.accountId ? String(initial.accountId) : '')
  const [endpointInput, setEndpointInput] = useState('')
  const [modelInput, setModelInput] = useState('')
  const [sinceInput, setSinceInput] = useState(() => (initial.since ? toDatetimeLocalValue(initial.since) : recentDatetimeLocal(24)))
  const [untilInput, setUntilInput] = useState(() => toDatetimeLocalValue(initial.until ?? new Date()))
  const [streamFilter, setStreamFilter] = useState<StreamFilter>('all')
  const [submitted, setSubmitted] = useState<UsageAccountRiskQuery>(() => ({
    timezone: TIMEZONE,
    windowKey: initial.windowKey,
    // 自定义窗口没带起止时与输入框默认值一致：最近 24 小时
    since: initial.windowKey === 'custom' ? (initial.since ?? new Date(Date.now() - 24 * 60 * 60 * 1000)).toISOString() : undefined,
    until: initial.windowKey === 'custom' ? (initial.until ?? new Date()).toISOString() : undefined,
    warningThresholdTokens: DEFAULT_WARNING_TOKENS,
    criticalThresholdTokens: DEFAULT_CRITICAL_TOKENS,
    accountId: initial.accountId,
    limit: 50,
  }))

  const riskQuery = useUsageDashboardAccountRisk(submitted)
  const data = riskQuery.data
  const maxBucketCount = useMemo(() => {
    if (!data?.buckets.length) return 0
    return Math.max(
      ...data.buckets.flatMap((bucket) => [
        bucket.rawReadCount,
        bucket.rawWriteCount,
        bucket.reportedReadCount,
        bucket.reportedWriteCount,
      ])
    )
  }, [data])

  const applyFilters = () => {
    const warningThresholdTokens = parsePositiveInt(warningInput, DEFAULT_WARNING_TOKENS)
    const criticalThresholdTokens = Math.max(
      warningThresholdTokens,
      parsePositiveInt(criticalInput, DEFAULT_CRITICAL_TOKENS)
    )
    const accountId = poolInput.trim() ? Number.parseInt(poolInput.trim(), 10) : undefined
    setSubmitted({
      timezone: TIMEZONE,
      windowKey,
      since: windowKey === 'custom' ? datetimeLocalToIso(sinceInput) : undefined,
      until: windowKey === 'custom' ? datetimeLocalToIso(untilInput) : undefined,
      warningThresholdTokens,
      criticalThresholdTokens,
      accountId: Number.isFinite(accountId) && accountId! > 0 ? accountId : undefined,
      endpoint: endpointInput.trim() || undefined,
      model: modelInput.trim() || undefined,
      stream: streamValueToQuery(streamFilter),
      limit: 50,
    })
  }

  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button variant="outline" onClick={() => riskQuery.refetch()} disabled={riskQuery.isFetching}>
            <RefreshCw className={cn(riskQuery.isFetching && 'animate-spin')} />
            刷新
          </Button>
        }
      />

      <SectionCard
        title="查询条件"
        description="只统计账号 usage，查询是只读聚合，不会修改调度或计费记录。"
        icon={<Filter />}
        actions={
          <Button size="sm" onClick={applyFilters} disabled={riskQuery.isFetching}>
            <Search />
            查询
          </Button>
        }
      >
        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-6">
          <Field label="时间">
            <Select value={windowKey} onValueChange={setWindowKey}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="today">今天</SelectItem>
                <SelectItem value="last24h">最近24小时</SelectItem>
                <SelectItem value="yesterday">昨天</SelectItem>
                <SelectItem value="last7d">最近7天</SelectItem>
                <SelectItem value="custom">自定义</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          {windowKey === 'custom' && (
            <>
              <Field label="开始时间">
                <Input type="datetime-local" value={sinceInput} onChange={(e) => setSinceInput(e.target.value)} />
              </Field>
              <Field label="结束时间">
                <Input type="datetime-local" value={untilInput} onChange={(e) => setUntilInput(e.target.value)} />
              </Field>
            </>
          )}
          <Field label="预警阈值">
            <Input value={warningInput} inputMode="numeric" onChange={(e) => setWarningInput(e.target.value)} />
          </Field>
          <Field label="严重阈值">
            <Input value={criticalInput} inputMode="numeric" onChange={(e) => setCriticalInput(e.target.value)} />
          </Field>
          <Field label="账号 ID">
            <Input placeholder="全部" value={poolInput} onChange={(e) => setPoolInput(e.target.value)} />
          </Field>
          <Field label="路径">
            <Input placeholder="全部" value={endpointInput} onChange={(e) => setEndpointInput(e.target.value)} />
          </Field>
          <Field label="流式">
            <Select value={streamFilter} onValueChange={(value) => setStreamFilter(value as StreamFilter)}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部</SelectItem>
                <SelectItem value="stream">流式</SelectItem>
                <SelectItem value="non_stream">非流式</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field label="模型" className="md:col-span-2 xl:col-span-2">
            <Input placeholder="全部" value={modelInput} onChange={(e) => setModelInput(e.target.value)} />
          </Field>
        </div>
        {data && (
          <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <Badge tone="info">窗口 {data.window.label}</Badge>
            <span>{formatDate(data.window.from)} - {formatDate(data.window.to)}</span>
            <Badge tone={data.thresholds.costFloorEnabled ? 'success' : 'neutral'}>
              成本目标 {data.thresholds.costFloorEnabled ? `上游 +${data.thresholds.costFloorMarginPercent}%` : '上游成本'}
            </Badge>
            <span>生成 {formatDate(data.generatedAt)}</span>
          </div>
        )}
      </SectionCard>

      {riskQuery.isLoading && <LoadingState text="加载账号风控数据..." />}
      {riskQuery.isError && (
        <ErrorState
          title="风控数据加载失败"
          message={extractErrorMessage(riskQuery.error)}
          action={<Button size="sm" variant="outline" onClick={() => riskQuery.refetch()}>重试</Button>}
        />
      )}

      {data && (
        <>
          <StatGrid min="12rem">
            <StatCard title="账号记录" value={formatCompact(data.totals.records)} valueTitle={formatNumber(data.totals.records)} desc={`成功 ${formatNumber(data.totals.successRecords)} / 失败 ${formatNumber(data.totals.errorRecords)}`} icon={<Boxes />} />
            <StatCard title="上游缓存最大" value={`${formatCompact(Math.max(data.rawCache.maxReadTokens, data.rawCache.maxWriteTokens))}`} valueTitle={tokenTitle(Math.max(data.rawCache.maxReadTokens, data.rawCache.maxWriteTokens))} desc={`读 ${formatCompact(data.rawCache.maxReadTokens)} / 写 ${formatCompact(data.rawCache.maxWriteTokens)}`} icon={<Gauge />} tone={toneForRisk(data.rawCache.eitherCriticalCount, data.rawCache.eitherWarningCount)} />
            <StatCard title="最终缓存最大" value={`${formatCompact(Math.max(data.reportedCache.maxReadTokens, data.reportedCache.maxWriteTokens))}`} valueTitle={tokenTitle(Math.max(data.reportedCache.maxReadTokens, data.reportedCache.maxWriteTokens))} desc={`读 ${formatCompact(data.reportedCache.maxReadTokens)} / 写 ${formatCompact(data.reportedCache.maxWriteTokens)}`} icon={<BarChart3 />} tone={toneForRisk(data.reportedCache.eitherCriticalCount, data.reportedCache.eitherWarningCount)} />
            <StatCard title="低于目标成本" value={formatCompact(data.cost.belowTargetCount)} valueTitle={formatNumber(data.cost.belowTargetCount)} desc={`差额 ${formatUsd(data.cost.totalTargetGapUsd)}`} icon={<DollarSign />} tone={data.cost.belowTargetCount > 0 ? 'error' : 'success'} />
            <StatCard title="成本利润" value={signedUsd(data.cost.profitUsd)} desc={`上游 ${formatUsd(data.cost.rawCostUsd)} / 最终 ${formatUsd(data.cost.reportedCostUsd)}`} icon={<DollarSign />} tone={data.cost.profitUsd < 0 ? 'error' : 'success'} />
            <StatCard title="输出为 0" value={formatCompact(data.totals.outputZeroRecords)} valueTitle={formatNumber(data.totals.outputZeroRecords)} desc={`缺计费 ${formatNumber(data.totals.missingAccountBillingRecords)}`} icon={<AlertTriangle />} tone={data.totals.outputZeroRecords > 0 ? 'warning' : 'success'} />
          </StatGrid>

          <div className="grid gap-5 xl:grid-cols-2">
            <CacheStatsCard title="上游 raw 缓存" stats={data.rawCache} />
            <CacheStatsCard title="最终 reported 缓存" stats={data.reportedCache} />
          </div>

          <SectionCard
            title="缓存分布"
            description="同一窗口内按 token 桶统计读缓存和写缓存，raw 是上游返回，reported 是最终返回调用方。"
            icon={<BarChart3 />}
          >
            <BucketTable buckets={data.buckets} maxCount={maxBucketCount} />
          </SectionCard>

          <SectionCard title="成本风控" description="目标成本按账号全局成本底线配置计算。" icon={<DollarSign />}>
            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
              <Metric label="上游 raw 成本" value={formatUsd(data.cost.rawCostUsd)} />
              <Metric label="最终 reported 成本" value={formatUsd(data.cost.reportedCostUsd)} />
              <Metric label="目标成本" value={formatUsd(data.cost.targetCostUsd)} />
              <Metric label="平均成本比例" value={costRatio(data.cost.avgCostRatio)} />
              <Metric label="低于上游成本" value={formatNumber(data.cost.belowRawCount)} tone={data.cost.belowRawCount > 0 ? 'bad' : 'good'} />
              <Metric label="低于目标成本" value={formatNumber(data.cost.belowTargetCount)} tone={data.cost.belowTargetCount > 0 ? 'bad' : 'good'} />
              <Metric label="最大亏损" value={formatUsd(data.cost.maxLossUsd)} tone={data.cost.maxLossUsd > 0 ? 'bad' : 'good'} />
              <Metric label="最大目标差额" value={formatUsd(data.cost.maxTargetGapUsd)} tone={data.cost.maxTargetGapUsd > 0 ? 'bad' : 'good'} />
            </div>
          </SectionCard>

          <div className="grid gap-5 xl:grid-cols-3">
            <GroupTable title="按账号" groups={data.byPool} />
            <GroupTable title="按路径" groups={data.byPath} />
            <GroupTable title="按模型" groups={data.byModel} />
          </div>

          <SectionCard
            title="风险样本"
            description="按严重程度、成本差额、缓存峰值排序，只返回有限样本，复制请求 ID 或直接去用量明细继续查。"
            icon={<AlertTriangle />}
          >
            <SamplesTable samples={data.samples} onOpenRecord={(id) => navigate(usageRecordLink(id))} />
          </SectionCard>

          <QualityHistoryCard accountId={submitted.accountId} from={data.window.from} to={data.window.to} />

          <AccountEventsTimeline
            since={data.window.from}
            until={data.window.to}
            accountId={submitted.accountId}
          />

          {data.totals.missingAccountBillingRecords > 0 && (
            <Callout tone="warning">
              当前窗口存在 {formatNumber(data.totals.missingAccountBillingRecords)} 条账号记录缺少 accountBilling。raw/reported 成本和缓存统计会因此不完整，需要优先查这些请求的记录链路。
            </Callout>
          )}
        </>
      )}
    </PageContainer>
  )
}

function Field({ label, className, children }: { label: string; className?: string; children: React.ReactNode }) {
  return (
    <label className={cn('grid gap-1.5 text-xs font-medium text-muted-foreground', className)}>
      {label}
      {children}
    </label>
  )
}

function Metric({ label, value, tone = 'neutral' }: { label: string; value: string; tone?: 'neutral' | 'good' | 'bad' }) {
  return (
    <div className="rounded-lg border bg-card px-3 py-2">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className={cn('mt-1 text-lg font-semibold tabular-nums', tone === 'good' && 'text-success', tone === 'bad' && 'text-destructive')}>
        {value}
      </div>
    </div>
  )
}

function CacheStatsCard({ title, stats }: { title: string; stats: UsageAccountRiskCacheStats }) {
  return (
    <SectionCard title={title} icon={<Gauge />}>
      <div className="grid gap-3 sm:grid-cols-2">
        <Metric label="读缓存最大/最小" value={`${formatCompact(stats.maxReadTokens)} / ${formatCompact(stats.minReadTokens)}`} />
        <Metric label="写缓存最大/最小" value={`${formatCompact(stats.maxWriteTokens)} / ${formatCompact(stats.minWriteTokens)}`} />
        <Metric label="读缓存均值" value={formatCompact(Math.round(stats.avgReadTokens))} />
        <Metric label="写缓存均值" value={formatCompact(Math.round(stats.avgWriteTokens))} />
        <Metric label="预警记录" value={formatNumber(stats.eitherWarningCount)} tone={stats.eitherWarningCount > 0 ? 'bad' : 'good'} />
        <Metric label="严重记录" value={formatNumber(stats.eitherCriticalCount)} tone={stats.eitherCriticalCount > 0 ? 'bad' : 'good'} />
      </div>
    </SectionCard>
  )
}

function BucketTable({ buckets, maxCount }: { buckets: UsageAccountRiskBucket[]; maxCount: number }) {
  if (!buckets.length) return <EmptyState title="暂无缓存分布" />
  return (
    <div className="overflow-x-auto">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>区间</TableHead>
            <TableHead>raw 读</TableHead>
            <TableHead>raw 写</TableHead>
            <TableHead>reported 读</TableHead>
            <TableHead>reported 写</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {buckets.map((bucket) => (
            <TableRow key={bucket.key}>
              <TableCell className="font-medium">{bucket.label}</TableCell>
              <BucketCell count={bucket.rawReadCount} max={maxCount} />
              <BucketCell count={bucket.rawWriteCount} max={maxCount} />
              <BucketCell count={bucket.reportedReadCount} max={maxCount} />
              <BucketCell count={bucket.reportedWriteCount} max={maxCount} />
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

function BucketCell({ count, max }: { count: number; max: number }) {
  const width = max > 0 ? `${Math.max(4, Math.round((count / max) * 100))}%` : '0%'
  return (
    <TableCell>
      <div className="flex min-w-[8rem] items-center gap-2">
        <div className="h-2 min-w-16 flex-1 overflow-hidden rounded-full bg-muted">
          <div className="h-full rounded-full bg-primary/70" style={{ width }} />
        </div>
        <span className="w-14 text-right text-xs tabular-nums text-muted-foreground">{formatCompact(count)}</span>
      </div>
    </TableCell>
  )
}

function GroupTable({ title, groups }: { title: string; groups: UsageAccountRiskGroup[] }) {
  return (
    <SectionCard title={title} bodyClassName="p-0">
      {!groups.length ? (
        <EmptyState title="暂无数据" className="rounded-none" />
      ) : (
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>对象</TableHead>
                <TableHead className="text-right">记录</TableHead>
                <TableHead className="text-right">风险</TableHead>
                <TableHead className="text-right">缓存峰值</TableHead>
                <TableHead className="text-right">目标差额</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {groups.map((group) => (
                <TableRow key={group.key}>
                  <TableCell className="max-w-[14rem] truncate font-medium" title={group.label}>{group.label}</TableCell>
                  <TableCell className="text-right tabular-nums">{formatCompact(group.records)}</TableCell>
                  <TableCell className="text-right">
                    <span className={cn('tabular-nums', group.criticalRecords > 0 ? 'text-destructive' : group.warningRecords > 0 ? 'text-warning' : 'text-success')}>
                      {formatCompact(group.criticalRecords)}/{formatCompact(group.warningRecords)}
                    </span>
                  </TableCell>
                  <TableCell className="text-right tabular-nums">
                    {formatCompact(Math.max(group.rawReadMax, group.rawWriteMax, group.reportedReadMax, group.reportedWriteMax))}
                  </TableCell>
                  <TableCell className={cn('text-right tabular-nums', group.totalTargetGapUsd > 0 && 'text-destructive')}>
                    {formatUsd(group.totalTargetGapUsd)}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </SectionCard>
  )
}

function SamplesTable({ samples, onOpenRecord }: { samples: UsageAccountRiskSample[]; onOpenRecord: (id: string) => void }) {
  if (!samples.length) return <EmptyState title="暂无风险样本" description="当前查询窗口没有命中缓存或成本风险样本。" />
  return (
    <div className="overflow-x-auto">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>时间 / 请求</TableHead>
            <TableHead>账号</TableHead>
            <TableHead>路径 / 模型</TableHead>
            <TableHead className="text-right">raw 输入/输出</TableHead>
            <TableHead className="text-right">raw 读/写</TableHead>
            <TableHead className="text-right">reported 输入/输出</TableHead>
            <TableHead className="text-right">reported 读/写</TableHead>
            <TableHead className="text-right">成本</TableHead>
            <TableHead>风险</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {samples.map((sample) => (
            <TableRow key={sample.id}>
              <TableCell>
                <div className="whitespace-nowrap text-xs text-muted-foreground">{formatDate(sample.createdAt)}</div>
                <div className="flex items-center gap-1">
                  <span className="max-w-[12rem] truncate font-mono text-xs" title={sample.id}>{sample.id}</span>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="shrink-0"
                    title="复制请求 ID"
                    aria-label="复制请求 ID"
                    onClick={() => void copyRequestId(sample.id)}
                  >
                    <Copy className="h-3.5 w-3.5" />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="shrink-0"
                    title="去用量明细查看这条请求"
                    aria-label="去用量明细"
                    onClick={() => onOpenRecord(sample.id)}
                  >
                    <ExternalLink className="h-3.5 w-3.5" />
                  </Button>
                </div>
              </TableCell>
              <TableCell>
                <div className="max-w-[11rem] truncate" title={poolLabel(sample.accountId, sample.accountName)}>
                  {poolLabel(sample.accountId, sample.accountName)}
                </div>
                <div className="text-xs text-muted-foreground">{sample.stream ? 'stream' : 'non-stream'}</div>
              </TableCell>
              <TableCell>
                <div className="max-w-[12rem] truncate" title={sample.endpoint}>{sample.endpoint}</div>
                <div className="max-w-[12rem] truncate text-xs text-muted-foreground" title={sample.pricingModel || sample.model}>
                  {sample.pricingModel || sample.model}
                </div>
              </TableCell>
              <TokenPairCell left={sample.rawInputTokens} right={sample.rawOutputTokens} />
              <TokenPairCell left={sample.rawCacheReadInputTokens} right={sample.rawCacheCreationInputTokens} />
              <TokenPairCell left={sample.reportedInputTokens} right={sample.reportedOutputTokens} />
              <TokenPairCell left={sample.reportedCacheReadInputTokens} right={sample.reportedCacheCreationInputTokens} />
              <TableCell className="text-right">
                <div className="tabular-nums">{formatUsd(sample.reportedCostUsd)}</div>
                <div className={cn('text-xs tabular-nums', sample.targetGapUsd > 0 ? 'text-destructive' : 'text-muted-foreground')}>
                  差 {formatUsd(sample.targetGapUsd)}
                </div>
              </TableCell>
              <TableCell>
                <div className="flex max-w-[14rem] flex-wrap gap-1">
                  {sample.riskReasons.map((reason) => (
                    <Badge key={reason} size="xs" tone={reason.includes('critical') || reason.includes('below') ? 'error' : 'warning'}>
                      {riskReasonLabel(reason)}
                    </Badge>
                  ))}
                </div>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

function TokenPairCell({ left, right }: { left: number; right: number }) {
  return (
    <TableCell className="text-right tabular-nums">
      <span title={`${formatNumber(left)} / ${formatNumber(right)}`}>{formatCompact(left)} / {formatCompact(right)}</span>
    </TableCell>
  )
}

// ─── 账号异动时间线 ─────────────────────────────────────────────────────────────

/** 时间线表格只列最近这么多条。 */
const EVENT_TABLE_LIMIT = 50
/** 老后端的汇总接口不认窗口：退回按明细统计，单次最多取这么多条，超过时计数只含最近的这些条。 */
const LEGACY_EVENT_COUNT_LIMIT = 500

/**
 * 当前窗口内的账号异动（冷却、观察期、自动禁用、额度耗尽等），复用审计接口。
 * 计数走汇总接口，按查询窗口 `[from, to)` 统计，填了账号 ID 时只统计该账号，和下面的表格同一口径；
 * 老后端（响应没有 until）只能从“现在”往前数小时，这时退回按窗口明细统计。
 */
function AccountEventsTimeline({ since, until, accountId }: { since: string; until: string; accountId?: number }) {
  const summaryQuery = useMemo(() => ({ since, until, accountId }), [accountId, since, until])
  const summary = useAccountEventWindowSummary(summaryQuery)
  const legacySummary = summary.data !== undefined && !summaryHonorsWindow(summary.data)
  const query = useMemo(
    () => ({
      page: 1,
      limit: legacySummary ? LEGACY_EVENT_COUNT_LIMIT : EVENT_TABLE_LIMIT,
      accountEvents: true,
      since,
      until,
      account: accountId ? `#${accountId}` : undefined,
    }),
    [accountId, legacySummary, since, until],
  )
  // 窗口起止是固定时间，刷新页面查询时窗口会换新，这里不需要定时重查。
  const events = useAuditLogsPage(query, false)
  const allRows = (events.data?.records ?? [])
    .map((record) => ({ record, event: accountEventView(record) }))
    .filter((item): item is { record: typeof item.record; event: NonNullable<typeof item.event> } => item.event !== null)
  const rows = allRows.slice(0, EVENT_TABLE_LIMIT)
  const tiles = summaryTiles(
    legacySummary ? summarizeAccountEvents(allRows.map((item) => item.event)) : summary.data?.items ?? [],
  )
  // 只有退回明细统计、且窗口内超过上限时，计数才是近似值。
  const countsTruncated = legacySummary && Boolean(events.data?.hasNext)
  const countsLoading = legacySummary ? events.isLoading && !events.data : summary.isLoading && !summary.data
  const countsError = legacySummary ? events.isError && events.error : summary.isError && summary.error
  const scope = accountId ? `账号 #${accountId}` : '全部账号'

  return (
    <SectionCard
      title="账号异动时间线"
      description={`当前窗口内最近 ${EVENT_TABLE_LIMIT} 条账号异动：进入冷却、进入观察期、自动禁用、额度耗尽等${accountId ? `，只看账号 #${accountId}` : ''}。`}
      icon={<FileClock />}
    >
      <div className="mb-3">
        <div className="mb-1.5 text-xs text-muted-foreground">
          按事件类型计数（当前窗口，{scope}{legacySummary ? '；后端版本较旧，按窗口明细统计' : ''}）
          {countsTruncated && `：窗口内超过 ${LEGACY_EVENT_COUNT_LIMIT} 条，只统计了最近 ${LEGACY_EVENT_COUNT_LIMIT} 条，完整计数到审计页按同一时间范围查看`}
        </div>
        {countsLoading ? null : countsError ? (
          <div className="text-xs text-destructive">读取失败：{extractErrorMessage(countsError)}</div>
        ) : tiles.length === 0 ? (
          <div className="text-xs text-muted-foreground">没有账号异动。</div>
        ) : (
          <div className="flex flex-wrap gap-2">
            {tiles.map((tile) => (
              <div key={tile.kind} className="flex items-center gap-2 rounded-md border px-2.5 py-1.5 text-xs">
                <Badge tone={tile.tone} size="xs">{tile.label}</Badge>
                <span className="font-mono tabular-nums">{countsTruncated ? '≥ ' : ''}{formatNumber(tile.events)} 次</span>
                <span className="text-muted-foreground">· {formatNumber(tile.accounts)} 个账号</span>
              </div>
            ))}
          </div>
        )}
      </div>
      {events.isLoading && !events.data ? (
        <LoadingState text="加载账号异动..." className="py-6" />
      ) : events.isError ? (
        <div className="text-xs text-destructive">读取失败：{extractErrorMessage(events.error)}</div>
      ) : rows.length === 0 ? (
        <EmptyState title="当前窗口没有账号异动" />
      ) : (
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>时间</TableHead>
                <TableHead>账号</TableHead>
                <TableHead>事件</TableHead>
                <TableHead>原因</TableHead>
                <TableHead>执行者</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map(({ record, event }) => (
                <TableRow key={record.id}>
                  <TableCell className="whitespace-nowrap text-xs tabular-nums text-muted-foreground">{formatDate(record.createdAt)}</TableCell>
                  <TableCell className="max-w-56 truncate text-xs font-medium" title={event.accountLabel}>
                    {event.accountLabel}
                    <span className="ml-1 text-[0.62rem] font-normal text-muted-foreground/70">#{event.accountId}</span>
                  </TableCell>
                  <TableCell><Badge tone={event.tone} size="xs">{event.label}</Badge></TableCell>
                  <TableCell className="max-w-72 truncate text-xs" title={event.reason}>{event.reason || '—'}</TableCell>
                  <TableCell className="whitespace-nowrap text-xs text-muted-foreground" title={record.actor}>{ACTOR_LABELS[record.actor] ?? record.actor}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {(events.data?.hasNext || allRows.length > rows.length) && (
            <p className="mt-2 text-[0.68rem] text-muted-foreground">只显示最近 {EVENT_TABLE_LIMIT} 条，更早的到审计页查看。</p>
          )}
        </div>
      )}
    </SectionCard>
  )
}

// ─── 质量分趋势 ─────────────────────────────────────────────────────────────────

const QUALITY_DESCRIPTION =
  '调度器被动采样的 EWMA 快照，每账号每分钟最多一个点，保留 7 天；质量分只在运行配置开启“质量感知调度”后才会采样。错误率越高、首字越慢，相对同批候选越容易进入观察期。'

/** 单个账号的调度质量分历史：失败率 EWMA、首字 / 耗时 EWMA，按查询窗口取数。 */
function QualityHistoryCard({ accountId, from, to }: { accountId?: number; from: string; to: string }) {
  const range = useMemo(() => ({ from, to }), [from, to])
  const history = useAccountQualityHistory(accountId, range)
  // 老后端不认 from/to（响应里没有 until），固定返回最近 24 小时：裁掉窗口外的点。
  const legacyBackend = Boolean(history.data && !history.data.until)
  const points = useMemo(() => pointsInWindow(history.data?.points ?? [], from, to), [from, history.data?.points, to])
  const rows = useMemo(() => qualityChartRows(points), [points])
  const withDate = rowsCrossDays(rows)
  const latest = points[points.length - 1]
  const hasTtft = points.some((point) => point.ttftEwmaMs != null)
  const timeTick = (value: string | number) => formatQualityTick(value, withDate)
  const bucket = bucketDescription(history.data?.bucketSecs)

  if (!accountId) {
    return (
      <SectionCard title="质量分趋势" description={QUALITY_DESCRIPTION} icon={<Activity />}>
        <EmptyState title="先在查询条件里填写账号 ID" description="质量分按单个账号查看，填写账号 ID 并点“查询”后显示该账号在当前窗口内的曲线。" />
      </SectionCard>
    )
  }

  return (
    <SectionCard
      title={`质量分趋势 · 账号 #${accountId}`}
      description={[QUALITY_DESCRIPTION, bucket ? `当前窗口${bucket}。` : ''].filter(Boolean).join('')}
      icon={<Activity />}
    >
      {history.isLoading && !history.data ? (
        <LoadingState text="加载质量分历史..." className="py-6" />
      ) : history.isError ? (
        <div className="text-xs text-destructive">读取失败：{extractErrorMessage(history.error)}</div>
      ) : points.length === 0 ? (
        <EmptyState
          title="窗口内没有质量采样"
          description="运行配置没有开启质量感知调度、该账号在窗口内没有流量，或窗口超出了 7 天保留期时都不会有采样。"
        />
      ) : (
        <>
          {legacyBackend && (
            <Callout tone="warning" className="mb-3">后端版本较旧，不支持按窗口查询，只显示最近 24 小时与当前窗口重叠的部分。</Callout>
          )}
          <div className="mb-3 grid gap-3 sm:grid-cols-3">
            <Metric label="最新失败率 EWMA" value={formatPercent(latest?.recentErrorRate ?? 0)} tone={(latest?.recentErrorRate ?? 0) > 0.05 ? 'bad' : 'good'} />
            <Metric label="最新首字 EWMA" value={formatMs(latest?.ttftEwmaMs)} />
            <Metric label="最新耗时 EWMA" value={formatMs(latest?.latencyEwmaMs)} />
          </div>
          <div className="grid gap-4 xl:grid-cols-2">
            <div>
              <div className="mb-1 text-xs text-muted-foreground">失败率 EWMA</div>
              <TrendAreaChart
                data={rows}
                xKey="t"
                height={200}
                series={[{ key: 'errorRate', name: '失败率 EWMA' }]}
                xTickFormatter={timeTick}
                yTickFormatter={formatRateTick}
                yAxisWidth={48}
                labelFormatter={formatQualityLabel}
                valueFormatter={(value) => formatPercent(Number(value))}
              />
            </div>
            <div>
              <div className="mb-1 text-xs text-muted-foreground">{hasTtft ? '首字 / 耗时 EWMA' : '耗时 EWMA（该账号没有流式首字采样）'}</div>
              <TrendAreaChart
                data={rows}
                xKey="t"
                height={200}
                series={hasTtft
                  ? [{ key: 'ttft', name: '首字 EWMA' }, { key: 'latency', name: '耗时 EWMA' }]
                  : [{ key: 'latency', name: '耗时 EWMA' }]}
                xTickFormatter={timeTick}
                yTickFormatter={formatMsTick}
                yAxisWidth={72}
                labelFormatter={formatQualityLabel}
                valueFormatter={(value) => formatMs(Number(value))}
              />
            </div>
          </div>
        </>
      )}
    </SectionCard>
  )
}
