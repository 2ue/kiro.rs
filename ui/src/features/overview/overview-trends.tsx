import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import {
  Area,
  Bar,
  CartesianGrid,
  ComposedChart,
  Line,
  ReferenceLine,
  XAxis,
  YAxis,
} from 'recharts'
import { Table2, BarChart3 } from 'lucide-react'
import { formatCompact, formatNumber, formatPercent, formatUsdFixed2 } from '@/lib/format'
import {
  CACHE_HIT_RATE_CRITICAL,
  CACHE_HIT_RATE_WARNING,
  cacheHitRate,
  sumCacheTokens,
  type CacheTokenMetrics,
} from './cache-hit-rate'
import { averagePerMinute, perMinute, weightedAverageDuration } from './overview-metrics'
import { cn } from '@/lib/utils'
import { EmptyState, LoadingState, SectionCard } from '@/components/patterns'
import {
  Button,
  ChartContainer,
  ChartTooltip,
  type ChartConfig,
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
// 槽位语义：0 蓝=主体，1 琥珀=需关注，2 青=恢复，3 玫红=失败，4 紫=其他。
const SLOT_COLORS = ['var(--trend-1)', 'var(--trend-2)', 'var(--trend-3)', 'var(--trend-4)', 'var(--trend-5)'] as const
const OTHER_COLOR = 'var(--trend-other)'
const INK_COLOR = 'var(--trend-line)'
const SURFACE = 'hsl(var(--card))'
const MAX_STACK_MEMBERS = 5

type ValueFormat = 'count' | 'usd' | 'percent' | 'ms'

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
  /** null 表示该时段无意义（例如没有输入 token），折线断开而不是画成 0 */
  value: (point: UsageOverviewSeriesPoint) => number | null
  /** 整个范围的汇总值；比例类折线必须先汇总分子分母再相除，不能对每个时段的比例求和 */
  aggregate?: (points: UsageOverviewSeriesPoint[]) => number | null
  /** 合计/参照类折线用虚线中性色，靠线型而不是颜色和分类系列区分 */
  reference?: boolean
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
  /** 图表顶部的大号汇总数；缺省为可见柱子的合计 */
  headline?: { label: string; value: (points: UsageOverviewSeriesPoint[]) => number | null }
}

function formatValue(value: number, format: ValueFormat): string {
  if (!Number.isFinite(value)) return '-'
  if (format === 'usd') return formatUsdFixed2(value)
  if (format === 'percent') return formatPercent(value)
  if (format === 'ms') return `${formatNumber(Math.round(value))} ms`
  return formatNumber(value)
}

function formatAxisValue(value: number, format: ValueFormat): string {
  if (format === 'usd') return `$${formatCompact(value)}`
  if (format === 'percent') return `${Math.round(value * 100)}%`
  if (format === 'ms') return value >= 1000 ? `${(value / 1000).toFixed(1)}s` : `${Math.round(value)}ms`
  return formatCompact(value)
}

/** 汇总数和图例用的紧凑格式；完整数值放在 title 里。 */
function formatSummaryValue(value: number | null, format: ValueFormat): string {
  if (value == null || !Number.isFinite(value)) return '-'
  if (format === 'usd') return Math.abs(value) >= 1000 ? `$${formatCompact(value)}` : formatUsdFixed2(value)
  if (format === 'count') return formatCompact(value)
  return formatValue(value, format)
}


function sumOver(points: UsageOverviewSeriesPoint[], value: (point: UsageOverviewSeriesPoint) => number | null): number {
  return points.reduce((sum, point) => sum + (value(point) ?? 0), 0)
}

function aggregateHitRate(
  points: UsageOverviewSeriesPoint[],
  pick: (point: UsageOverviewSeriesPoint) => CacheTokenMetrics | undefined,
): number | null {
  return cacheHitRate(sumCacheTokens(points.map(pick).filter((item): item is CacheTokenMetrics => Boolean(item))))
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
 */
function stackSeries(
  groups: UsageOverviewSeriesGroup[],
  rows: (point: UsageOverviewSeriesPoint) => UsageOverviewSeriesBreakdown[] | undefined,
  pick: (metrics: UsageOverviewSeriesMetrics) => number,
): TrendSeries[] {
  const head = groups.slice(0, MAX_STACK_MEMBERS)
  const tail = groups.slice(MAX_STACK_MEMBERS)
  const series: TrendSeries[] = head.map((group, index) => ({
    key: group.key,
    name: group.label,
    color: SLOT_COLORS[index] ?? OTHER_COLOR,
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

// ─── 两组视图定义 ─────────────────────────────────────────────────────────────

const HIT_RATE_FOOTNOTE = `缓存读 ÷（未缓存输入 + 缓存读 + 缓存写），与 sub2api 口径一致，按 token 加权。参考线：低于 ${Math.round(CACHE_HIT_RATE_WARNING * 100)}% 需关注，低于 ${Math.round(CACHE_HIT_RATE_CRITICAL * 100)}% 偏低。`
const HIT_RATE_REFERENCE = { value: CACHE_HIT_RATE_WARNING, label: '关注线' }

/** 每个成员一条命中率折线；比例不能堆叠，超过 5 个成员时只画前 5 个。 */
function hitRateLines(
  groups: UsageOverviewSeriesGroup[],
  rows: (point: UsageOverviewSeriesPoint) => UsageOverviewSeriesBreakdown[] | undefined,
): TrendLine[] {
  return stackSeries(groups.slice(0, MAX_STACK_MEMBERS), rows, () => 0).map((series) => ({
    key: `hit:${series.key}`,
    name: series.name,
    color: series.color,
    value: (point) => {
      const row = rows(point)?.find((item) => item.key === series.key)
      return row ? cacheHitRate(row.metrics) : null
    },
    aggregate: (points) => aggregateHitRate(points, (point) => rows(point)?.find((item) => item.key === series.key)?.metrics),
  }))
}

/** 汇总分区：整体请求、对外计费、Token、缓存命中率、耗时；小时粒度再加 RPM / TPM。 */
export function summaryTrendViews(options: { hourly?: boolean } = {}): TrendView[] {
  const errorRow: TrendExtraRow = { label: '错误率', value: (point) => errorRate(point.all) }
  const views: TrendView[] = [
    {
      key: 'requests',
      label: '请求',
      format: 'count',
      bars: [
        { key: 'success', name: '成功', color: SLOT_COLORS[2], value: (point) => point.all.successRequests ?? 0 },
        { key: 'errors', name: '错误', color: SLOT_COLORS[3], value: (point) => point.all.errorRequests },
      ],
      lines: [{
        key: 'requests',
        name: '请求合计',
        color: INK_COLOR,
        reference: true,
        value: (point) => point.all.requests,
        aggregate: (points) => sumOver(points, (point) => point.all.requests),
      }],
      extraRows: [errorRow],
      headline: { label: '请求合计', value: (points) => sumOver(points, (point) => point.all.requests) },
      footnote: '柱子按成功 / 错误堆叠，虚线是请求合计（含状态未知的请求时会高于两者之和）。',
    },
    {
      key: 'duration',
      label: '耗时',
      format: 'ms',
      bars: [],
      lines: [{
        key: 'avgDuration',
        name: '平均耗时',
        color: SLOT_COLORS[1],
        value: (point) => (point.all.requests > 0 ? point.all.averageDurationMs ?? 0 : null),
        aggregate: (points) => weightedAverageDuration(points, (point) => point.all),
      }],
      headline: { label: '平均耗时', value: (points) => weightedAverageDuration(points, (point) => point.all) },
      footnote: '每个时段内请求的平均总耗时（从收到请求到响应结束），汇总按请求数加权。',
    },
    {
      key: 'revenue',
      label: '对外计费',
      format: 'usd',
      bars: [{ key: 'billable', name: '可计费', color: SLOT_COLORS[0], value: (point) => point.all.billableCostUsd }],
      lines: [{
        key: 'rawCost',
        name: '上游成本',
        color: INK_COLOR,
        reference: true,
        value: (point) => point.all.rawCostUsd,
        aggregate: (points) => sumOver(points, (point) => point.all.rawCostUsd),
      }],
      extraRows: [{ label: '毛利', value: (point) => formatUsdFixed2(point.all.profitUsd) }],
      footnote: '柱子是按最终上报 usage 对下游的可计费金额，虚线是账号实际花出去的上游成本。',
    },
    {
      key: 'tokens',
      label: 'Token',
      format: 'count',
      bars: [
        { key: 'input', name: '输入', color: SLOT_COLORS[2], value: (point) => point.all.inputTokens },
        { key: 'output', name: '输出', color: SLOT_COLORS[4], value: (point) => point.all.outputTokens },
        { key: 'cacheRead', name: '缓存读', color: SLOT_COLORS[0], value: (point) => point.all.cacheReadInputTokens },
        { key: 'cacheWrite', name: '缓存写', color: SLOT_COLORS[1], value: (point) => point.all.cacheCreationInputTokens },
      ],
    },
    {
      key: 'cacheHitRate',
      label: '缓存命中率',
      format: 'percent',
      bars: [],
      ratioAxis: true,
      reference: HIT_RATE_REFERENCE,
      lines: [
        { key: 'hit:all', name: '整体', color: SLOT_COLORS[0], value: (point) => cacheHitRate(point.all), aggregate: (points) => aggregateHitRate(points, (point) => point.all) },
      ],
      headline: { label: '整体命中率', value: (points) => aggregateHitRate(points, (point) => point.all) },
      footnote: HIT_RATE_FOOTNOTE,
    },
  ]
  if (options.hourly) {
    views.push(
      {
        key: 'rpm',
        label: 'RPM',
        format: 'count',
        bars: [],
        lines: [{
          key: 'rpm',
          name: '每分钟请求',
          color: SLOT_COLORS[0],
          value: (point) => perMinute(point.all.requests),
          aggregate: (points) => averagePerMinute(points, (point) => point.all.requests),
        }],
        headline: {
          label: '平均 RPM',
          value: (points) => averagePerMinute(points, (point) => point.all.requests),
        },
        footnote: '每个小时桶的请求数 ÷ 60，是该小时的平均每分钟请求；进行中的小时偏低，折线在那里断开。',
      },
      {
        key: 'tpm',
        label: 'TPM',
        format: 'count',
        bars: [],
        lines: [{
          key: 'tpm',
          name: '每分钟 Token',
          color: SLOT_COLORS[4],
          value: (point) => perMinute(point.all.inputTokens + point.all.outputTokens),
          aggregate: (points) => averagePerMinute(points, (point) => point.all.inputTokens + point.all.outputTokens),
        }],
        headline: {
          label: '平均 TPM',
          value: (points) => averagePerMinute(points, (point) => point.all.inputTokens + point.all.outputTokens),
        },
        footnote: '每个小时桶的（输入 + 输出）token ÷ 60；缓存读写不计入。',
      },
    )
  }
  return views
}

/** 账号分区：按账号堆叠的上游成本、毛利、请求与命中率。 */
export function accountTrendViews(groups: UsageOverviewSeriesGroups): TrendView[] {
  const byAccount = (pick: (metrics: UsageOverviewSeriesMetrics) => number) => stackSeries(
    groups.accounts,
    (point) => point.accounts,
    pick,
  )
  return [
    {
      key: 'rawCost',
      label: '上游成本',
      format: 'usd',
      bars: byAccount((m) => m.rawCostUsd),
      lines: [{
        key: 'billable',
        name: '可计费',
        color: INK_COLOR,
        reference: true,
        value: (point) => point.external.billableCostUsd,
        aggregate: (points) => sumOver(points, (point) => point.external.billableCostUsd),
      }],
      extraRows: [{ label: '毛利', value: (point) => formatUsdFixed2(point.external.profitUsd) }],
      footnote: '柱子是各账号实际花出去的上游成本，虚线是对下游的可计费金额。',
    },
    {
      key: 'profit',
      label: '毛利',
      format: 'usd',
      bars: [{ key: 'profit', name: '毛利', color: SLOT_COLORS[2], value: (point) => point.external.profitUsd }],
      footnote: '补偿后计费 - 上游原始成本；低于 0 的时段在亏钱。',
    },
    {
      key: 'requests',
      label: '请求',
      format: 'count',
      bars: byAccount((m) => m.requests),
      extraRows: [{ label: '错误率', value: (point) => errorRate(point.external) }],
    },
    {
      key: 'cacheHitRate',
      label: '缓存命中率',
      format: 'percent',
      bars: [],
      ratioAxis: true,
      reference: HIT_RATE_REFERENCE,
      lines: hitRateLines(groups.accounts, (point) => point.accounts),
      headline: { label: '账号命中率', value: (points) => aggregateHitRate(points, (point) => point.external) },
      footnote: HIT_RATE_FOOTNOTE,
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
    <div className="grid min-w-44 gap-1.5 rounded-lg border border-border/50 bg-background px-2.5 py-1.5 text-xs shadow-xl">
      <div className="flex items-center justify-between gap-3">
        <span className="font-medium text-foreground">{label}</span>
        {payloadIndex >= partialFrom && (
          <span className="rounded-full bg-muted px-1.5 py-0.5 text-[0.62rem] text-muted-foreground">进行中</span>
        )}
      </div>
      <div className="grid gap-1.5">
        {[...bars].reverse().map((series) => (
          <div key={series.key} className="flex items-center gap-2 leading-none">
            <span className="size-2.5 shrink-0 rounded-[2px]" style={{ background: series.color }} />
            <span className="truncate text-muted-foreground">{series.name}</span>
            <span className="ml-auto pl-3 font-mono font-medium tabular-nums text-foreground">{formatValue(series.value(point), view.format)}</span>
          </div>
        ))}
        {lines.map((line) => {
          const lineValue = line.value(point)
          return (
            <div key={line.key} className="flex items-center gap-2 leading-none">
              <span className="h-2.5 w-1 shrink-0 rounded-[2px]" style={{ background: line.color }} />
              <span className="truncate text-muted-foreground">{line.name}</span>
              <span className="ml-auto pl-3 font-mono font-medium tabular-nums text-foreground">
                {lineValue == null ? '-' : formatValue(lineValue, view.format)}
              </span>
            </div>
          )
        })}
      </div>
      {(bars.length > 1 || view.extraRows?.length) && (
        <div className="grid gap-1.5 border-t border-border/50 pt-1.5">
          {bars.length > 1 && (
            <div className="flex items-center gap-2 leading-none">
              <span className="text-muted-foreground">合计</span>
              <span className="ml-auto font-mono font-medium tabular-nums text-foreground">{formatValue(total, view.format)}</span>
            </div>
          )}
          {view.extraRows?.map((row) => (
            <div key={row.label} className="flex items-center gap-2 leading-none">
              <span className="text-muted-foreground">{row.label}</span>
              <span className="ml-auto font-mono font-medium tabular-nums text-foreground">{row.value(point)}</span>
            </div>
          ))}
        </div>
      )}
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

/** 跟随系统“减少动态效果”设置；开启时图表不做过渡动画。 */
function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() =>
    typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches,
  )
  useEffect(() => {
    const query = window.matchMedia('(prefers-reduced-motion: reduce)')
    const onChange = () => setReduced(query.matches)
    query.addEventListener('change', onChange)
    return () => query.removeEventListener('change', onChange)
  }, [])
  return reduced
}

/** 分段切换器：选中底块在选项之间滑动，而不是瞬间跳过去。 */
function SegmentedViews({
  views,
  value,
  onChange,
}: {
  views: Array<{ key: string; label: string }>
  value: string
  onChange: (key: string) => void
}) {
  const listRef = useRef<HTMLDivElement>(null)
  const [indicator, setIndicator] = useState<{ left: number; width: number } | null>(null)
  useLayoutEffect(() => {
    const list = listRef.current
    const active = list?.querySelector<HTMLButtonElement>(`[data-view="${CSS.escape(value)}"]`)
    if (!list || !active) return
    setIndicator({ left: active.offsetLeft, width: active.offsetWidth })
  }, [value, views])
  return (
    <div
      ref={listRef}
      role="tablist"
      aria-label="切换视图"
      className="relative inline-flex h-8 max-w-full items-center overflow-x-auto rounded-lg bg-muted p-1 text-muted-foreground"
    >
      {indicator && (
        <span
          aria-hidden
          className="absolute top-1 bottom-1 rounded-md bg-card shadow-sm ring-1 ring-border/40 transition-[left,width] duration-300 ease-out motion-reduce:transition-none"
          style={{ left: indicator.left, width: indicator.width }}
        />
      )}
      {views.map((item) => {
        const selected = item.key === value
        return (
          <Button variant="ghost"
            key={item.key}
            type="button"
            role="tab"
            data-view={item.key}
            aria-selected={selected}
            className={cn(
              'relative z-10 inline-flex h-6 items-center whitespace-nowrap rounded-md px-2.5 text-xs font-medium transition-colors duration-200',
              'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              selected ? 'text-foreground' : 'hover:text-foreground',
            )}
            onClick={() => onChange(item.key)}
          >
            {item.label}
          </Button>
        )
      })}
    </div>
  )
}

/**
 * 图表里的数据键按“第几个柱子 / 第几条线”固定，而不是按系列名。
 * 切换视图时 recharts 看到的是同一组柱子和折线，只是数值变了，
 * 于是会从旧高度平滑过渡到新高度，而不是整张图卸载重画。
 */
const barSlot = (index: number) => `b${index}`
const partialSlot = (index: number) => `b${index}p`
const lineSlot = (index: number) => `l${index}`

function chartVar(slot: string): string {
  return `var(--color-${slot})`
}

/** 柱子/折线下方的渐变，从实色淡到透明。 */
function SeriesGradients({ id, items }: { id: string; items: Array<{ key: string; area?: boolean }> }) {
  return (
    <defs>
      {items.map((item, index) => (
        <linearGradient key={item.key} id={`${id}-${index}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="5%" stopColor={chartVar(item.key)} stopOpacity={item.area ? 0.3 : 0.95} />
          <stop offset="95%" stopColor={chartVar(item.key)} stopOpacity={item.area ? 0.02 : 0.75} />
        </linearGradient>
      ))}
    </defs>
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
  const gradientId = useId().replace(/:/g, '')
  const reducedMotion = usePrefersReducedMotion()
  // 数据变化（切换视图、隐藏系列、自动刷新）时柱高和折线位置做补间。
  const animation = { isAnimationActive: !reducedMotion, animationDuration: 500, animationEasing: 'ease-in-out' as const }
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
      view.bars.forEach((series, slot) => {
        const value = series.value(point)
        // 未结束的时段单独成一组淡色柱子，避免看起来像流量暴跌。
        row[barSlot(slot)] = index >= partialFrom ? 0 : value
        row[partialSlot(slot)] = index >= partialFrom ? value : 0
      })
      // 未结束的时段数值天然偏低，折线在这里断开；提示框和数据表照常显示。
      ;(view.lines ?? []).forEach((line, slot) => {
        row[lineSlot(slot)] = index >= partialFrom ? null : line.value(point)
      })
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
  const lineOnly = view.bars.length === 0
  const legendItems = [
    ...view.bars.map((series) => ({
      key: series.key,
      name: series.name,
      color: series.color,
      shape: 'dot' as const,
      dashed: false,
      total: sumOver(points, series.value),
    })),
    ...(view.lines ?? []).map((line) => ({
      key: line.key,
      name: line.name,
      color: line.color,
      shape: 'line' as const,
      dashed: Boolean(line.reference),
      total: line.aggregate ? line.aggregate(points) : null,
    })),
  ]
  const headlineValue = view.headline
    ? view.headline.value(points)
    : visibleBars.reduce((sum, series) => sum + sumOver(points, series.value), 0)
  const headlineLabel = view.headline?.label ?? `${view.label}合计`
  const barSlots = view.bars
    .map((series, slot) => ({ series, slot }))
    .filter(({ series }) => !hidden.has(series.key))
  const lineSlots = (view.lines ?? [])
    .map((line, slot) => ({ line, slot }))
    .filter(({ line }) => !hidden.has(line.key))
  const topBarSlot = barSlots[barSlots.length - 1]?.slot
  const hasData = points.length > 0 && !view.emptyHint
  const xAxis = xAxisTicks(points, hourly)
  // 命中率可能整体很低或很高；固定 0~100% 会把折线压成一条线，这里按数据留白，并封顶在 0~1。
  const ratioDomain: [(min: number) => number, (max: number) => number] = [
    (min) => Math.max(0, Math.floor((min - 0.05) * 20) / 20),
    (max) => Math.min(1, Math.ceil((max + 0.05) * 20) / 20),
  ]
  // 柱子用实色 + 卡片色描边分段，避免堆叠时渐变在接缝处断层；渐变只给折线下的面积用。
  const gradientItems = lineSlots.map(({ slot }) => ({ key: lineSlot(slot), area: true }))
  const chartConfig: ChartConfig = Object.fromEntries([
    ...view.bars.map((series, slot) => [barSlot(slot), { label: series.name, color: series.color }]),
    ...(view.lines ?? []).map((line, slot) => [lineSlot(slot), { label: line.name, color: line.color }]),
  ])
  const gradientRef = (slot: string) => `url(#${gradientId}-${gradientItems.findIndex((item) => item.key === slot)})`
  // 只有单条折线时铺渐变面积；多条叠在一起会发灰、看不清。
  const areaLineSlot = lineOnly && lineSlots.length === 1 ? lineSlots[0]?.slot : undefined

  return (
    <SectionCard
      title={title}
      icon={icon}
      actions={
        <div className="flex flex-wrap items-center gap-1.5">
          {actions}
          <Button
            size="icon-xs"
            variant="ghost"
            className="text-muted-foreground"
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
      <div className="mb-4 flex flex-wrap items-end justify-between gap-3">
        <div className="min-w-0">
          <div className="text-xs text-muted-foreground">{headlineLabel}</div>
          <div
            key={view.key}
            className="mt-0.5 text-2xl font-semibold tracking-tight text-foreground animate-in fade-in-0 slide-in-from-bottom-1 duration-300 motion-reduce:animate-none"
            title={headlineValue == null ? undefined : formatValue(headlineValue, view.format)}
          >
            {formatSummaryValue(headlineValue, view.format)}
          </div>
        </div>
        {views.length > 1 && <SegmentedViews views={views} value={view.key} onChange={switchView} />}
      </div>
      <div className={cn('transition-opacity', loading && 'opacity-60')}>
        {loading && points.length === 0 ? (
          <LoadingState text="加载趋势..." className="py-10" />
        ) : !hasData ? (
          <EmptyState title={view.emptyHint ?? '暂无趋势数据'} className="py-10" />
        ) : showTable ? (
          <TrendTable points={points} view={view} bars={visibleBars} lines={visibleLines} partialFrom={partialFrom} />
        ) : (
          <ChartContainer
            config={chartConfig}
            className="aspect-auto h-[280px] w-full [&_.recharts-curve]:transition-colors [&_.recharts-curve]:duration-500 [&_.recharts-rectangle]:transition-colors [&_.recharts-rectangle]:duration-500 motion-reduce:[&_.recharts-curve]:transition-none motion-reduce:[&_.recharts-rectangle]:transition-none"
            role="img"
            aria-label={`${title}：${view.label}，共 ${points.length} 个时段，可切换为数据表查看具体数值`}
          >
              <ComposedChart
                data={rows}
                margin={{ top: 12, right: 12, bottom: 0, left: 0 }}
                barCategoryGap={points.length > 48 ? '12%' : '28%'}
                onClick={(state) => {
                  const index = Number(state?.activeTooltipIndex)
                  const point = Number.isInteger(index) ? points[index] : undefined
                  if (point && onDrillDown) onDrillDown(point)
                }}
                className={onDrillDown ? 'cursor-pointer' : undefined}
              >
                <SeriesGradients id={gradientId} items={gradientItems} />
                <CartesianGrid vertical={false} />
                <XAxis
                  dataKey="label"
                  ticks={xAxis.ticks}
                  interval={xAxis.ticks ? 0 : 'preserveStartEnd'}
                  tickFormatter={xAxis.format}
                  tickLine={false}
                  axisLine={false}
                  tickMargin={10}
                  minTickGap={24}
                />
                <YAxis
                  domain={view.ratioAxis ? ratioDomain : [0, 'auto']}
                  tickFormatter={(value: number) => formatAxisValue(value, view.format)}
                  tickLine={false}
                  axisLine={false}
                  tickMargin={6}
                  tickCount={5}
                  width={52}
                />
                <ChartTooltip
                  cursor={lineOnly ? { strokeDasharray: '3 3' } : { radius: 6, fillOpacity: 0.6 }}
                  isAnimationActive={false}
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
                    strokeOpacity={0.45}
                    strokeDasharray="2 4"
                    ifOverflow="extendDomain"
                    label={{
                      value: `${view.reference.label} ${formatValue(view.reference.value, view.format)}`,
                      position: 'insideTopRight',
                      fill: 'hsl(var(--muted-foreground))',
                      fontSize: 11,
                    }}
                  />
                )}
                {barSlots.map(({ series, slot }) => (
                  <Bar
                    key={barSlot(slot)}
                    dataKey={barSlot(slot)}
                    name={series.name}
                    stackId="trend"
                    fill={chartVar(barSlot(slot))}
                    fillOpacity={0.9}
                    stroke={SURFACE}
                    strokeWidth={barSlots.length > 1 ? 1 : 0}
                    maxBarSize={points.length > 48 ? 10 : 28}
                    radius={slot === topBarSlot ? [5, 5, 0, 0] : 0}
                    activeBar={{ fillOpacity: 1 }}
                    {...animation}
                  />
                ))}
                {barSlots.map(({ series, slot }) => (
                  <Bar
                    key={partialSlot(slot)}
                    dataKey={partialSlot(slot)}
                    name={`${series.name}（进行中）`}
                    stackId="trend"
                    fill={chartVar(barSlot(slot))}
                    fillOpacity={0.25}
                    stroke={SURFACE}
                    strokeWidth={barSlots.length > 1 ? 1 : 0}
                    maxBarSize={points.length > 48 ? 10 : 28}
                    radius={slot === topBarSlot ? [5, 5, 0, 0] : 0}
                    legendType="none"
                    {...animation}
                  />
                ))}
                {areaLineSlot != null && (
                  <Area
                    key={`${lineSlot(areaLineSlot)}a`}
                    dataKey={lineSlot(areaLineSlot)}
                    type="monotone"
                    stroke="none"
                    fill={gradientRef(lineSlot(areaLineSlot))}
                    connectNulls={false}
                    activeDot={false}
                    legendType="none"
                    tooltipType="none"
                    {...animation}
                  />
                )}
                {lineSlots.map(({ line, slot }) => (
                  <Line
                    key={lineSlot(slot)}
                    dataKey={lineSlot(slot)}
                    name={line.name}
                    type={points.length > 72 ? 'linear' : 'monotone'}
                    stroke={chartVar(lineSlot(slot))}
                    strokeWidth={line.reference ? 1.75 : points.length > 72 ? 1.5 : 2}
                    strokeDasharray={line.reference ? '5 4' : undefined}
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    dot={false}
                    activeDot={{ r: 4, fill: chartVar(lineSlot(slot)), stroke: SURFACE, strokeWidth: 2 }}
                    connectNulls={false}
                    {...animation}
                  />
                ))}
              </ComposedChart>
          </ChartContainer>
        )}
      </div>
      {hasData && legendItems.length > 1 && (
        <div
          key={view.key}
          className="mt-4 flex flex-wrap items-center gap-x-5 gap-y-2 border-t border-border/50 pt-3 animate-in fade-in-0 duration-300 motion-reduce:animate-none"
        >
          {legendItems.map((item) => {
            const off = hidden.has(item.key)
            return (
              <Button variant="ghost"
                key={item.key}
                type="button"
                className={cn(
                  'group inline-flex items-center gap-2 rounded-md text-left text-xs transition-opacity duration-200',
                  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
                  off && 'opacity-40',
                )}
                aria-pressed={!off}
                title={off ? `显示 ${item.name}` : `隐藏 ${item.name}`}
                onClick={() => toggle(item.key)}
              >
                {item.dashed ? (
                  <span className="w-3.5 shrink-0 border-t-2 border-dashed" style={{ borderColor: item.color }} />
                ) : (
                  <span
                    className={cn('shrink-0', item.shape === 'line' ? 'h-0.5 w-3.5 rounded-full' : 'size-2.5 rounded-full')}
                    style={{ background: item.color }}
                  />
                )}
                <span className="text-muted-foreground group-hover:text-foreground">{item.name}</span>
                {item.total != null && (
                  <span className="font-medium tabular-nums text-foreground">{formatSummaryValue(item.total, view.format)}</span>
                )}
              </Button>
            )
          })}
        </div>
      )}
      {hasData && view.footnote && (
        <p className="mt-3 text-[0.68rem] leading-relaxed text-muted-foreground">{view.footnote}</p>
      )}
    </SectionCard>
  )
}
