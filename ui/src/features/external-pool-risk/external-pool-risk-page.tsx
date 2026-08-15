import { useMemo, useState } from 'react'
import {
  AlertTriangle,
  BarChart3,
  Boxes,
  DollarSign,
  Filter,
  Gauge,
  RefreshCw,
  Search,
} from 'lucide-react'
import { useUsageDashboardExternalPoolRisk } from '@/hooks/use-usage'
import { formatCompact, formatDate, formatNumber, formatPercent, formatUsd } from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import type {
  UsageExternalPoolRiskBucket,
  UsageExternalPoolRiskCacheStats,
  UsageExternalPoolRiskGroup,
  UsageExternalPoolRiskQuery,
  UsageExternalPoolRiskSample,
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
  if (!id && !name) return '未知外部池'
  return [`#${id ?? '-'}`, name].filter(Boolean).join(' ')
}

function riskReasonLabel(reason: string): string {
  const labels: Record<string, string> = {
    missing_external_pool_billing: '缺少计费记录',
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

export function ExternalPoolRiskPage() {
  const [windowKey, setWindowKey] = useState('last24h')
  const [warningInput, setWarningInput] = useState(String(DEFAULT_WARNING_TOKENS))
  const [criticalInput, setCriticalInput] = useState(String(DEFAULT_CRITICAL_TOKENS))
  const [poolInput, setPoolInput] = useState('')
  const [endpointInput, setEndpointInput] = useState('')
  const [modelInput, setModelInput] = useState('')
  const [sinceInput, setSinceInput] = useState(() => recentDatetimeLocal(24))
  const [untilInput, setUntilInput] = useState(() => toDatetimeLocalValue(new Date()))
  const [streamFilter, setStreamFilter] = useState<StreamFilter>('all')
  const [submitted, setSubmitted] = useState<UsageExternalPoolRiskQuery>({
    timezone: TIMEZONE,
    windowKey: 'last24h',
    warningThresholdTokens: DEFAULT_WARNING_TOKENS,
    criticalThresholdTokens: DEFAULT_CRITICAL_TOKENS,
    limit: 50,
  })

  const riskQuery = useUsageDashboardExternalPoolRisk(submitted)
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
    const externalPoolId = poolInput.trim() ? Number.parseInt(poolInput.trim(), 10) : undefined
    setSubmitted({
      timezone: TIMEZONE,
      windowKey,
      since: windowKey === 'custom' ? datetimeLocalToIso(sinceInput) : undefined,
      until: windowKey === 'custom' ? datetimeLocalToIso(untilInput) : undefined,
      warningThresholdTokens,
      criticalThresholdTokens,
      externalPoolId: Number.isFinite(externalPoolId) && externalPoolId! > 0 ? externalPoolId : undefined,
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
          <Button size="sm" variant="outline" onClick={() => riskQuery.refetch()} disabled={riskQuery.isFetching}>
            <RefreshCw className={cn(riskQuery.isFetching && 'animate-spin')} />
            刷新
          </Button>
        }
      />

      <SectionCard
        title="查询条件"
        description="只统计外部池 usage，查询是只读聚合，不会修改调度或计费记录。"
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
          <Field label="外部池 ID">
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

      {riskQuery.isLoading && <LoadingState text="加载外部池风控数据..." />}
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
            <StatCard title="外部池记录" value={formatCompact(data.totals.records)} valueTitle={formatNumber(data.totals.records)} desc={`成功 ${formatNumber(data.totals.successRecords)} / 失败 ${formatNumber(data.totals.errorRecords)}`} icon={<Boxes />} />
            <StatCard title="上游缓存最大" value={`${formatCompact(Math.max(data.rawCache.maxReadTokens, data.rawCache.maxWriteTokens))}`} valueTitle={tokenTitle(Math.max(data.rawCache.maxReadTokens, data.rawCache.maxWriteTokens))} desc={`读 ${formatCompact(data.rawCache.maxReadTokens)} / 写 ${formatCompact(data.rawCache.maxWriteTokens)}`} icon={<Gauge />} tone={toneForRisk(data.rawCache.eitherCriticalCount, data.rawCache.eitherWarningCount)} />
            <StatCard title="最终缓存最大" value={`${formatCompact(Math.max(data.reportedCache.maxReadTokens, data.reportedCache.maxWriteTokens))}`} valueTitle={tokenTitle(Math.max(data.reportedCache.maxReadTokens, data.reportedCache.maxWriteTokens))} desc={`读 ${formatCompact(data.reportedCache.maxReadTokens)} / 写 ${formatCompact(data.reportedCache.maxWriteTokens)}`} icon={<BarChart3 />} tone={toneForRisk(data.reportedCache.eitherCriticalCount, data.reportedCache.eitherWarningCount)} />
            <StatCard title="低于目标成本" value={formatCompact(data.cost.belowTargetCount)} valueTitle={formatNumber(data.cost.belowTargetCount)} desc={`差额 ${formatUsd(data.cost.totalTargetGapUsd)}`} icon={<DollarSign />} tone={data.cost.belowTargetCount > 0 ? 'error' : 'success'} />
            <StatCard title="成本利润" value={signedUsd(data.cost.profitUsd)} desc={`上游 ${formatUsd(data.cost.rawCostUsd)} / 最终 ${formatUsd(data.cost.reportedCostUsd)}`} icon={<DollarSign />} tone={data.cost.profitUsd < 0 ? 'error' : 'success'} />
            <StatCard title="输出为 0" value={formatCompact(data.totals.outputZeroRecords)} valueTitle={formatNumber(data.totals.outputZeroRecords)} desc={`缺计费 ${formatNumber(data.totals.missingExternalPoolBillingRecords)}`} icon={<AlertTriangle />} tone={data.totals.outputZeroRecords > 0 ? 'warning' : 'success'} />
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

          <SectionCard title="成本风控" description="目标成本按外部池全局成本底线配置计算。" icon={<DollarSign />}>
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
            <GroupTable title="按外部池" groups={data.byPool} />
            <GroupTable title="按路径" groups={data.byPath} />
            <GroupTable title="按模型" groups={data.byModel} />
          </div>

          <SectionCard
            title="风险样本"
            description="按严重程度、成本差额、缓存峰值排序，只返回有限样本，复制请求 ID 可去用量明细继续查。"
            icon={<AlertTriangle />}
          >
            <SamplesTable samples={data.samples} />
          </SectionCard>

          {data.totals.missingExternalPoolBillingRecords > 0 && (
            <Callout tone="warning">
              当前窗口存在 {formatNumber(data.totals.missingExternalPoolBillingRecords)} 条外部池记录缺少 externalPoolBilling。raw/reported 成本和缓存统计会因此不完整，需要优先查这些请求的记录链路。
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

function CacheStatsCard({ title, stats }: { title: string; stats: UsageExternalPoolRiskCacheStats }) {
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

function BucketTable({ buckets, maxCount }: { buckets: UsageExternalPoolRiskBucket[]; maxCount: number }) {
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

function GroupTable({ title, groups }: { title: string; groups: UsageExternalPoolRiskGroup[] }) {
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

function SamplesTable({ samples }: { samples: UsageExternalPoolRiskSample[] }) {
  if (!samples.length) return <EmptyState title="暂无风险样本" description="当前查询窗口没有命中缓存或成本风险样本。" />
  return (
    <div className="overflow-x-auto">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>时间 / 请求</TableHead>
            <TableHead>外部池</TableHead>
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
                <div className="max-w-[12rem] truncate font-mono text-xs" title={sample.id}>{sample.id}</div>
              </TableCell>
              <TableCell>
                <div className="max-w-[11rem] truncate" title={poolLabel(sample.externalPoolId, sample.externalPoolName)}>
                  {poolLabel(sample.externalPoolId, sample.externalPoolName)}
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
