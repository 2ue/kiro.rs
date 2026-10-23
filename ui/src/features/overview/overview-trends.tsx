import { useMemo, useState, type ReactNode } from 'react'
import {
  Bar,
  CartesianGrid,
  Cell,
  ComposedChart,
  Line,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip as ChartTooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { Table2, BarChart3 } from 'lucide-react'
import { formatCompact, formatCredits, formatNumber, formatPercent, formatUsdFixed2 } from '@/lib/format'
import { CACHE_HIT_RATE_CRITICAL, CACHE_HIT_RATE_WARNING, cacheHitRate } from './cache-hit-rate'
import { cn } from '@/lib/utils'
import { EmptyState, LoadingState, SectionCard } from '@/components/patterns'
import {
  Button,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'
import type {
  UsageOverviewSeriesBreakdown,
  UsageOverviewSeriesGroup,
  UsageOverviewSeriesGroups,
  UsageOverviewSeriesMetrics,
  UsageOverviewSeriesPoint,
} from '@/types/api'

// 分类色按位分配、不循环；超过 5 个成员折叠为“其他”。见 globals.css 的 --trend-*。
const SLOT_COLORS = ['var(--trend-1)', 'var(--trend-2)', 'var(--trend-3)', 'var(--trend-4)', 'var(--trend-5)'] as const
const OTHER_COLOR = 'var(--trend-other)'
const INK_COLOR = 'var(--trend-line)'
const SURFACE = 'hsl(var(--card))'
const MAX_STACK_MEMBERS = 5

// 颜色跟随实体而不是排名：已知成员固定槽位，切换范围不会互换颜色。
const LOCAL_ROUTE_COLORS: Record<string, string> = {
  local_success: SLOT_COLORS[0],
  local_error_no_fallback: SLOT_COLORS[1],
  local_rescue_after_external: SLOT_COLORS[2],
}
const EXTERNAL_REASON_COLORS: Record<string, string> = {
  policy_direct: SLOT_COLORS[0],
  local_capacity: SLOT_COLORS[1],
  local_unavailable: SLOT_COLORS[2],
  model_unsupported: SLOT_COLORS[3],
  system_degraded: SLOT_COLORS[4],
}

type ValueFormat = 'count' | 'usd' | 'credits' | 'usdPerCredit' | 'percent'

type TrendSeries = {
  key: string
  name: string
  color: string
  value: (point: UsageOverviewSeriesPoint) => number
}

type TrendLine = {
  key: string
  name: string
  color: string
  /** null 表示该时段无意义（例如没有消耗积分），折线断开而不是画成 0 */
  value: (point: UsageOverviewSeriesPoint) => number | null
}

type TrendExtraRow = {
  label: string
  value: (point: UsageOverviewSeriesPoint) => string
}

export type TrendView = {
  key: string
  label: string
  format: ValueFormat
  bars: TrendSeries[]
  /** 叠加的折线；比例类视图（命中率）只有折线、没有柱子 */
  lines?: TrendLine[]
  reference?: { value: number; label: string }
  /** 比例类视图：y 轴按数据自适应，但不超出 0~1 */
  ratioAxis?: boolean
  extraRows?: TrendExtraRow[]
  emptyHint?: string
  footnote?: ReactNode
}

function formatValue(value: number, format: ValueFormat): string {
  if (!Number.isFinite(value)) return '-'
  if (format === 'usd') return formatUsdFixed2(value)
  if (format === 'credits') return formatCredits(value)
  if (format === 'usdPerCredit') return `$${value.toFixed(3)}`
  if (format === 'percent') return formatPercent(value)
  return formatNumber(value)
}

function formatAxisValue(value: number, format: ValueFormat): string {
  if (format === 'usd') return `$${formatCompact(value)}`
  if (format === 'usdPerCredit') return `$${value.toFixed(2)}`
  if (format === 'percent') return `${Math.round(value * 100)}%`
  return formatCompact(value)
}

function totalTokens(metrics: UsageOverviewSeriesMetrics): number {
  return metrics.inputTokens + metrics.outputTokens + metrics.cacheReadInputTokens + metrics.cacheCreationInputTokens
}

function errorRate(metrics: UsageOverviewSeriesMetrics): string {
  return metrics.requests > 0 ? formatPercent(metrics.errorRequests / metrics.requests) : '-'
}

function breakdownValue(
  rows: UsageOverviewSeriesBreakdown[] | undefined,
  key: string,
  pick: (metrics: UsageOverviewSeriesMetrics) => number,
): number {
  const row = rows?.find((item) => item.key === key)
  return row ? pick(row.metrics) : 0
}

/**
 * 把一个维度的成员展开成堆叠系列：前 5 个各占一个槽位，其余合并成“其他”。
 * `fixedColors` 给已知成员固定颜色；未知成员按出现顺序取剩余槽位。
 */
function stackSeries(
  groups: UsageOverviewSeriesGroup[],
  rows: (point: UsageOverviewSeriesPoint) => UsageOverviewSeriesBreakdown[] | undefined,
  pick: (metrics: UsageOverviewSeriesMetrics) => number,
  fixedColors?: Record<string, string>,
): TrendSeries[] {
  const head = groups.slice(0, MAX_STACK_MEMBERS)
  const tail = groups.slice(MAX_STACK_MEMBERS)
  const used = new Set(head.map((group) => fixedColors?.[group.key]).filter(Boolean))
  const free = SLOT_COLORS.filter((color) => !used.has(color))
  let nextFree = 0
  const series: TrendSeries[] = head.map((group) => ({
    key: group.key,
    name: group.label,
    color: fixedColors?.[group.key] ?? free[nextFree++] ?? OTHER_COLOR,
    value: (point) => breakdownValue(rows(point), group.key, pick),
  }))
  if (tail.length > 0) {
    const tailKeys = new Set(tail.map((group) => group.key))
    series.push({
      key: '__other__',
      name: `其他 ${tail.length} 个`,
      color: OTHER_COLOR,
      value: (point) => (rows(point) ?? [])
        .filter((row) => tailKeys.has(row.key))
        .reduce((sum, row) => sum + pick(row.metrics), 0),
    })
  }
  return series
}

const HIT_RATE_FOOTNOTE = `缓存读 ÷（未缓存输入 + 缓存读 + 缓存写），与 sub2api 口径一致，按 token 加权。参考线：低于 ${Math.round(CACHE_HIT_RATE_WARNING * 100)}% 需关注，低于 ${Math.round(CACHE_HIT_RATE_CRITICAL * 100)}% 偏低。`
const HIT_RATE_REFERENCE = { value: CACHE_HIT_RATE_WARNING, label: '关注线' }

/** 每个成员一条命中率折线；比例不能堆叠，超过 5 个成员时只画前 5 个。 */
function hitRateLines(
  groups: UsageOverviewSeriesGroup[],
  rows: (point: UsageOverviewSeriesPoint) => UsageOverviewSeriesBreakdown[] | undefined,
  fixedColors?: Record<string, string>,
): TrendLine[] {
  return stackSeries(groups.slice(0, MAX_STACK_MEMBERS), rows, () => 0, fixedColors).map((series) => ({
    key: `hit:${series.key}`,
    name: series.name,
    color: series.color,
    value: (point) => {
      const row = rows(point)?.find((item) => item.key === series.key)
      return row ? cacheHitRate(row.metrics) : null
    },
  }))
}

// ─── 三个分区的视图定义 ─────────────────────────────────────────────────────────

export function summaryTrendViews(creditUnitPriceUsd: number | null): TrendView[] {
  const splitBars = (pick: (metrics: UsageOverviewSeriesMetrics) => number): TrendSeries[] => [
    { key: 'local', name: '本地账号池', color: SLOT_COLORS[0], value: (point) => pick(point.local) },
    { key: 'external', name: '外部池', color: SLOT_COLORS[1], value: (point) => pick(point.external) },
  ]
  const errorRow: TrendExtraRow = { label: '错误率', value: (point) => errorRate(point.all) }
  const views: TrendView[] = [
    { key: 'requests', label: '请求', format: 'count', bars: splitBars((m) => m.requests), extraRows: [errorRow] },
    {
      key: 'revenue',
      label: '对外计费',
      format: 'usd',
      bars: splitBars((m) => m.estimatedCostUsd),
      footnote: '本地为估算计费，外部池为最终可计费金额，两者都是对下游收的钱，可以直接相加。',
    },
    { key: 'tokens', label: 'Token', format: 'count', bars: splitBars(totalTokens) },
    {
      key: 'cacheHitRate',
      label: '缓存命中率',
      format: 'percent',
      bars: [],
      ratioAxis: true,
      reference: HIT_RATE_REFERENCE,
      lines: [
        { key: 'hit:all', name: '整体', color: INK_COLOR, value: (point) => cacheHitRate(point.all) },
        { key: 'hit:local', name: '本地账号池', color: SLOT_COLORS[0], value: (point) => cacheHitRate(point.local) },
        { key: 'hit:external', name: '外部池', color: SLOT_COLORS[1], value: (point) => cacheHitRate(point.external) },
      ],
      footnote: HIT_RATE_FOOTNOTE,
    },
  ]
  if (creditUnitPriceUsd != null) {
    const localCost = (point: UsageOverviewSeriesPoint) => point.local.kiroMeteringUsage * creditUnitPriceUsd
    const cost = (point: UsageOverviewSeriesPoint) => localCost(point) + point.external.rawCostUsd
    views.push({
      key: 'margin',
      label: '成本与毛利',
      format: 'usd',
      bars: [
        { key: 'localCost', name: '本地积分成本', color: SLOT_COLORS[0], value: localCost },
        { key: 'externalCost', name: '外部上游成本', color: SLOT_COLORS[1], value: (point) => point.external.rawCostUsd },
      ],
      lines: [{ key: 'revenue', name: '对外计费', color: INK_COLOR, value: (point) => point.all.estimatedCostUsd }],
      extraRows: [
        { label: '毛利', value: (point) => formatUsdFixed2(point.all.estimatedCostUsd - cost(point)) },
      ],
      footnote: `本地成本 = 积分 × 积分单价 $${creditUnitPriceUsd}；折线高于柱子表示该时段赚钱。`,
    })
  }
  return views
}

export function localTrendViews(groups: UsageOverviewSeriesGroups, creditUnitPriceUsd: number | null): TrendView[] {
  const hasRoutes = groups.localRoutes.length > 0
  const byRoute = (pick: (metrics: UsageOverviewSeriesMetrics) => number): TrendSeries[] => (hasRoutes
    ? stackSeries(groups.localRoutes, (point) => point.localRoutes, pick, LOCAL_ROUTE_COLORS)
    : [{ key: 'local', name: '本地账号池', color: SLOT_COLORS[0], value: (point) => pick(point.local) }])
  const routeHint = hasRoutes
    ? undefined
    : '当前范围没有路由拆分数据。历史记录需要离线运行 maintenance usage-route-dimension-backfill 回灌，已按本地合计展示。'
  return [
    { key: 'credits', label: '积分', format: 'credits', bars: byRoute((m) => m.kiroMeteringUsage), footnote: routeHint },
    {
      key: 'requests',
      label: '请求',
      format: 'count',
      bars: byRoute((m) => m.requests),
      extraRows: [{ label: '错误率', value: (point) => errorRate(point.local) }],
      footnote: routeHint,
    },
    {
      key: 'cacheHitRate',
      label: '缓存命中率',
      format: 'percent',
      bars: [],
      ratioAxis: true,
      reference: HIT_RATE_REFERENCE,
      lines: hasRoutes
        ? hitRateLines(groups.localRoutes, (point) => point.localRoutes, LOCAL_ROUTE_COLORS)
        : [{ key: 'hit:local', name: '本地账号池', color: SLOT_COLORS[0], value: (point) => cacheHitRate(point.local) }],
      footnote: routeHint ?? HIT_RATE_FOOTNOTE,
    },
    {
      key: 'revenuePerCredit',
      label: '每积分收入',
      format: 'usdPerCredit',
      bars: [],
      lines: [{
        key: 'revenuePerCredit',
        name: '每积分收入',
        color: SLOT_COLORS[0],
        value: (point) => (point.local.kiroMeteringUsage > 0
          ? point.local.estimatedCostUsd / point.local.kiroMeteringUsage
          : null),
      }],
      reference: creditUnitPriceUsd != null ? { value: creditUnitPriceUsd, label: '积分单价' } : undefined,
      footnote: creditUnitPriceUsd != null
        ? '估算计费 ÷ 消耗积分。折线低于积分单价参考线的时段，本地账号在亏钱。'
        : '估算计费 ÷ 消耗积分。在“运行配置 → 请求容量”填写积分单价后会显示盈亏参考线。',
    },
  ]
}

export function externalTrendViews(groups: UsageOverviewSeriesGroups): TrendView[] {
  const byPool = (pick: (metrics: UsageOverviewSeriesMetrics) => number) => stackSeries(
    groups.externalPools,
    (point) => point.externalPools,
    pick,
  )
  return [
    {
      key: 'rawCost',
      label: '上游成本',
      format: 'usd',
      bars: byPool((m) => m.rawCostUsd),
      lines: [{ key: 'billable', name: '可计费', color: INK_COLOR, value: (point) => point.external.billableCostUsd }],
      extraRows: [{ label: '毛利', value: (point) => formatUsdFixed2(point.external.profitUsd) }],
      footnote: '柱子是外部池实际花出去的上游成本，折线是对下游的可计费金额。',
    },
    {
      key: 'profit',
      label: '毛利',
      format: 'usd',
      bars: [{ key: 'profit', name: '毛利', color: SLOT_COLORS[0], value: (point) => point.external.profitUsd }],
      footnote: '补偿后计费 - 上游原始成本；低于 0 的时段在亏钱。',
    },
    {
      key: 'cacheHitRate',
      label: '缓存命中率',
      format: 'percent',
      bars: [],
      ratioAxis: true,
      reference: HIT_RATE_REFERENCE,
      lines: hitRateLines(groups.externalPools, (point) => point.externalPools),
      footnote: HIT_RATE_FOOTNOTE,
    },
    {
      key: 'reasonHitRate',
      label: '原因命中率',
      format: 'percent',
      bars: [],
      ratioAxis: true,
      reference: HIT_RATE_REFERENCE,
      lines: hitRateLines(groups.externalReasons, (point) => point.externalReasons, EXTERNAL_REASON_COLORS),
      emptyHint: groups.externalReasons.length === 0
        ? '当前范围没有原因数据。历史记录需要离线运行 maintenance usage-route-dimension-backfill 回灌。'
        : undefined,
      footnote: '按走外部的原因拆开命中率：从本地溢出到外部的请求会丢失会话粘性，命中率明显低于策略直连时说明溢出在损失缓存。',
    },
    {
      key: 'reasons',
      label: '走外部原因',
      format: 'count',
      bars: stackSeries(groups.externalReasons, (point) => point.externalReasons, (m) => m.requests, EXTERNAL_REASON_COLORS),
      extraRows: [{ label: '错误率', value: (point) => errorRate(point.external) }],
      emptyHint: groups.externalReasons.length === 0
        ? '当前范围没有原因数据。历史记录需要离线运行 maintenance usage-route-dimension-backfill 回灌。'
        : undefined,
      footnote: '按“该谁处理”归类：策略直连是主动选择；容量不足要加账号或调并发；账号不可用要查账号健康。',
    },
  ]
}

// ─── 图表 ───────────────────────────────────────────────────────────────────────

type ChartRow = Record<string, number | string | boolean | null>

/**
 * 小时粒度跨多天时，只在 00:00（显示日期）和 12:00 打刻度，
 * 否则滚动窗口从 01:00 开始，自动刻度永远落不到日界。
 */
function xAxisTicks(points: UsageOverviewSeriesPoint[], hourly: boolean) {
  if (!hourly) return { ticks: undefined, format: (value: string) => value }
  const multiDay = points.length > 24
  const format = (value: string) => {
    const [day, time] = value.split(' ')
    if (!multiDay) return time ?? value
    return time === '00:00' ? day ?? value : time ?? value
  }
  if (!multiDay) return { ticks: undefined, format }
  const ticks = points
    .map((point) => point.label)
    .filter((label) => label.endsWith(' 00:00') || label.endsWith(' 12:00'))
  return { ticks, format }
}

function TrendTooltip({
  active,
  label,
  payloadIndex,
  points,
  view,
  bars,
  lines,
  partialFrom,
}: {
  active?: boolean
  label?: string | number
  payloadIndex?: number
  points: UsageOverviewSeriesPoint[]
  view: TrendView
  bars: TrendSeries[]
  lines: TrendLine[]
  partialFrom: number
}) {
  if (!active || payloadIndex == null) return null
  const point = points[payloadIndex]
  if (!point) return null
  const total = bars.reduce((sum, series) => sum + series.value(point), 0)
  return (
    <div className="min-w-48 rounded-lg bg-popover/95 px-3 py-2 text-xs shadow-md ring-1 ring-border/60 backdrop-blur">
      <div className="mb-1.5 font-medium text-popover-foreground">
        {label}
        {payloadIndex >= partialFrom && <span className="ml-1 text-muted-foreground">（进行中）</span>}
      </div>
      <div className="space-y-1">
        {[...bars].reverse().map((series) => (
          <div key={series.key} className="flex items-center gap-2">
            <span className="h-0.5 w-3 shrink-0 rounded-full" style={{ background: series.color }} />
            <span className="text-muted-foreground">{series.name}</span>
            <span className="ml-auto font-semibold tabular-nums text-popover-foreground">{formatValue(series.value(point), view.format)}</span>
          </div>
        ))}
        {bars.length > 1 && (
          <div className="flex items-center gap-2 border-t border-border/60 pt-1">
            <span className="pl-5 text-muted-foreground">合计</span>
            <span className="ml-auto font-semibold tabular-nums text-popover-foreground">{formatValue(total, view.format)}</span>
          </div>
        )}
        {lines.map((line) => {
          const lineValue = line.value(point)
          return (
            <div key={line.key} className="flex items-center gap-2">
              <span className="h-0.5 w-3 shrink-0 rounded-full" style={{ background: line.color }} />
              <span className="text-muted-foreground">{line.name}</span>
              <span className="ml-auto font-semibold tabular-nums text-popover-foreground">
                {lineValue == null ? '-' : formatValue(lineValue, view.format)}
              </span>
            </div>
          )
        })}
        {view.extraRows?.map((row) => (
          <div key={row.label} className="flex items-center gap-2">
            <span className="pl-5 text-muted-foreground">{row.label}</span>
            <span className="ml-auto font-semibold tabular-nums text-popover-foreground">{row.value(point)}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

function TrendTable({
  points,
  view,
  bars,
  lines,
  partialFrom,
}: {
  points: UsageOverviewSeriesPoint[]
  view: TrendView
  bars: TrendSeries[]
  lines: TrendLine[]
  partialFrom: number
}) {
  return (
    <div className="max-h-[320px] overflow-auto">
      <Table className="min-w-[560px] text-xs">
        <TableHeader>
          <TableRow>
            <TableHead>时段</TableHead>
            {bars.map((series) => <TableHead key={series.key} className="text-right">{series.name}</TableHead>)}
            {bars.length > 1 && <TableHead className="text-right">合计</TableHead>}
            {lines.map((line) => <TableHead key={line.key} className="text-right">{line.name}</TableHead>)}
            {view.extraRows?.map((row) => <TableHead key={row.label} className="text-right">{row.label}</TableHead>)}
          </TableRow>
        </TableHeader>
        <TableBody>
          {points.map((point, index) => {
            return (
              <TableRow key={point.bucketStart}>
                <TableCell className="whitespace-nowrap">
                  {point.label}
                  {index >= partialFrom && <span className="ml-1 text-muted-foreground">进行中</span>}
                </TableCell>
                {bars.map((series) => (
                  <TableCell key={series.key} className="text-right font-mono tabular-nums">{formatValue(series.value(point), view.format)}</TableCell>
                ))}
                {bars.length > 1 && (
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatValue(bars.reduce((sum, series) => sum + series.value(point), 0), view.format)}
                  </TableCell>
                )}
                {lines.map((line) => {
                  const lineValue = line.value(point)
                  return (
                    <TableCell key={line.key} className="text-right font-mono tabular-nums">
                      {lineValue == null ? '-' : formatValue(lineValue, view.format)}
                    </TableCell>
                  )
                })}
                {view.extraRows?.map((row) => (
                  <TableCell key={row.label} className="text-right font-mono tabular-nums">{row.value(point)}</TableCell>
                ))}
              </TableRow>
            )
          })}
        </TableBody>
      </Table>
    </div>
  )
}

export function OverviewTrendCard({
  title,
  icon,
  views,
  points,
  hourly,
  partialFrom,
  loading,
  actions,
  onDrillDown,
}: {
  title: string
  icon: ReactNode
  views: TrendView[]
  points: UsageOverviewSeriesPoint[]
  hourly: boolean
  /** 从该下标开始的时段还没结束（当前小时/当天），柱子降低不透明度 */
  partialFrom: number
  loading?: boolean
  actions?: ReactNode
  onDrillDown?: (point: UsageOverviewSeriesPoint) => void
}) {
  const [viewKey, setViewKey] = useState(views[0]?.key ?? '')
  const [hidden, setHidden] = useState<Set<string>>(() => new Set())
  const [showTable, setShowTable] = useState(false)
  const view = views.find((item) => item.key === viewKey) ?? views[0]

  const visibleBars = useMemo(
    () => (view?.bars ?? []).filter((series) => !hidden.has(series.key)),
    [hidden, view],
  )
  const visibleLines = useMemo(
    () => (view?.lines ?? []).filter((line) => !hidden.has(line.key)),
    [hidden, view],
  )
  const rows = useMemo<ChartRow[]>(() => {
    if (!view) return []
    return points.map((point, index) => {
      const row: ChartRow = { label: point.label, index }
      for (const series of view.bars) row[series.key] = series.value(point)
      // 未结束的时段数值天然偏低，折线在这里断开，避免画出一条“暴跌”；提示框和数据表照常显示。
      for (const line of view.lines ?? []) row[line.key] = index >= partialFrom ? null : line.value(point)
      return row
    })
  }, [partialFrom, points, view])

  if (!view) return null

  const switchView = (key: string) => {
    setViewKey(key)
    setHidden(new Set())
  }
  const toggle = (key: string) => setHidden((prev) => {
    const next = new Set(prev)
    if (next.has(key)) next.delete(key)
    else next.add(key)
    return next
  })
  const legendItems = [
    ...view.bars.map((series) => ({ key: series.key, name: series.name, color: series.color, shape: 'rect' as const })),
    ...(view.lines ?? []).map((line) => ({ key: line.key, name: line.name, color: line.color, shape: 'line' as const })),
  ]
  const topBarKey = visibleBars[visibleBars.length - 1]?.key
  const hasData = points.length > 0 && !view.emptyHint
  const xAxis = xAxisTicks(points, hourly)
  // 命中率可能整体很低或很高；固定 0~100% 会把折线压成一条线，这里按数据留白，并封顶在 0~1。
  const ratioDomain: [(min: number) => number, (max: number) => number] = [
    (min) => Math.max(0, Math.floor((min - 0.05) * 20) / 20),
    (max) => Math.min(1, Math.ceil((max + 0.05) * 20) / 20),
  ]

  return (
    <SectionCard
      title={title}
      icon={icon}
      actions={
        <div className="flex flex-wrap items-center gap-1">
          {views.length > 1 && views.map((item) => (
            <Button
              key={item.key}
              size="xs"
              variant={item.key === view.key ? 'secondary' : 'ghost'}
              aria-pressed={item.key === view.key}
              onClick={() => switchView(item.key)}
            >
              {item.label}
            </Button>
          ))}
          {actions}
          <Button
            size="icon-xs"
            variant="ghost"
            title={showTable ? '切换为图表' : '切换为数据表'}
            aria-label={showTable ? '切换为图表' : '切换为数据表'}
            aria-pressed={showTable}
            onClick={() => setShowTable((value) => !value)}
          >
            {showTable ? <BarChart3 /> : <Table2 />}
          </Button>
        </div>
      }
    >
      <div className={cn('transition-opacity', loading && 'opacity-60')}>
        {loading && points.length === 0 ? (
          <LoadingState text="加载趋势..." className="py-10" />
        ) : !hasData ? (
          <EmptyState title={view.emptyHint ?? '暂无趋势数据'} className="py-10" />
        ) : showTable ? (
          <TrendTable points={points} view={view} bars={visibleBars} lines={visibleLines} partialFrom={partialFrom} />
        ) : (
          <div
            className="h-[260px] w-full"
            role="img"
            aria-label={`${title}：${view.label}，共 ${points.length} 个时段，可切换为数据表查看具体数值`}
          >
            <ResponsiveContainer width="100%" height="100%">
              <ComposedChart
                data={rows}
                margin={{ top: 8, right: 8, bottom: 0, left: 0 }}
                barCategoryGap="18%"
                onClick={(state) => {
                  const index = Number(state?.activeTooltipIndex)
                  const point = Number.isInteger(index) ? points[index] : undefined
                  if (point && onDrillDown) onDrillDown(point)
                }}
                className={onDrillDown ? 'cursor-pointer' : undefined}
              >
                <CartesianGrid stroke="hsl(var(--chart-grid))" vertical={false} />
                <XAxis
                  dataKey="label"
                  ticks={xAxis.ticks}
                  interval={xAxis.ticks ? 0 : 'preserveStartEnd'}
                  tickFormatter={xAxis.format}
                  stroke="hsl(var(--muted-foreground))"
                  fontSize={11}
                  tickLine={false}
                  axisLine={{ stroke: 'hsl(var(--border))' }}
                  minTickGap={16}
                />
                <YAxis
                  domain={view.ratioAxis ? ratioDomain : [0, 'auto']}
                  tickFormatter={(value: number) => formatAxisValue(value, view.format)}
                  stroke="hsl(var(--muted-foreground))"
                  fontSize={11}
                  tickLine={false}
                  axisLine={false}
                  width={56}
                />
                <ChartTooltip
                  cursor={{ fill: 'hsl(var(--muted) / 0.5)' }}
                  content={(props) => (
                    <TrendTooltip
                      active={props.active}
                      label={props.label as string | number | undefined}
                      payloadIndex={Number.isInteger(Number(props.activeIndex)) ? Number(props.activeIndex) : undefined}
                      points={points}
                      view={view}
                      bars={visibleBars}
                      lines={visibleLines}
                      partialFrom={partialFrom}
                    />
                  )}
                />
                {view.reference && (
                  <ReferenceLine
                    y={view.reference.value}
                    stroke="hsl(var(--muted-foreground))"
                    strokeWidth={1}
                    label={{
                      value: `${view.reference.label} ${formatValue(view.reference.value, view.format)}`,
                      position: 'insideTopRight',
                      fill: 'hsl(var(--muted-foreground))',
                      fontSize: 11,
                    }}
                  />
                )}
                {visibleBars.map((series) => (
                  <Bar
                    key={series.key}
                    dataKey={series.key}
                    name={series.name}
                    stackId="trend"
                    fill={series.color}
                    stroke={SURFACE}
                    strokeWidth={1}
                    maxBarSize={24}
                    radius={series.key === topBarKey ? [4, 4, 0, 0] : 0}
                    isAnimationActive={false}
                  >
                    {rows.map((row) => (
                      <Cell key={String(row.index)} fillOpacity={Number(row.index) >= partialFrom ? 0.45 : 1} />
                    ))}
                  </Bar>
                ))}
                {visibleLines.map((line) => (
                  <Line
                    key={line.key}
                    dataKey={line.key}
                    name={line.name}
                    stroke={line.color}
                    strokeWidth={2}
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    dot={false}
                    activeDot={{ r: 4, stroke: SURFACE, strokeWidth: 2 }}
                    connectNulls={false}
                    isAnimationActive={false}
                  />
                ))}
              </ComposedChart>
            </ResponsiveContainer>
          </div>
        )}
      </div>
      {hasData && legendItems.length > 1 && (
        <div className="mt-3 flex flex-wrap items-center gap-1.5">
          {legendItems.map((item) => {
            const off = hidden.has(item.key)
            return (
              <Button
                key={item.key}
                size="xs"
                variant={off ? 'ghost' : 'secondary'}
                className="h-6 gap-1.5 px-2 text-[0.68rem]"
                aria-pressed={!off}
                onClick={() => toggle(item.key)}
              >
                <span
                  className={cn(item.shape === 'line' ? 'h-0.5 w-3 rounded-full' : 'size-2 rounded-sm', off && 'opacity-30')}
                  style={{ background: item.color }}
                />
                <span className={cn(off && 'text-muted-foreground line-through')}>{item.name}</span>
              </Button>
            )
          })}
        </div>
      )}
      {hasData && view.footnote && (
        <p className="mt-2 text-[0.68rem] leading-relaxed text-muted-foreground">{view.footnote}</p>
      )}
    </SectionCard>
  )
}
