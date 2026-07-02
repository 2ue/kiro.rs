import * as React from 'react'
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { cn } from '@/lib/utils'

/**
 * 基于 recharts 的图表套件,颜色全部走设计系统 CSS 变量(--chart-*),
 * 自动适配亮/暗主题。封装常用图表,统一坐标轴/网格/提示框样式。
 */

export const CHART_COLORS = [
  'hsl(var(--chart-1))',
  'hsl(var(--chart-2))',
  'hsl(var(--chart-3))',
  'hsl(var(--chart-4))',
  'hsl(var(--chart-5))',
  'hsl(var(--chart-6))',
  'hsl(var(--chart-7))',
  'hsl(var(--chart-8))',
]

const axisProps = {
  stroke: 'hsl(var(--muted-foreground))',
  fontSize: 11,
  tickLine: false,
  axisLine: false,
} as const

interface TooltipPayloadItem {
  name?: string
  value?: number | string
  color?: string
  dataKey?: string | number
}

function ChartTooltip({
  active,
  payload,
  label,
  formatter,
  labelFormatter,
}: {
  active?: boolean
  payload?: TooltipPayloadItem[]
  label?: string | number
  formatter?: (value: number | string, name: string) => React.ReactNode
  labelFormatter?: (label: string | number) => React.ReactNode
}) {
  if (!active || !payload?.length) return null
  return (
    <div className="rounded-lg bg-popover/95 px-3 py-2 text-xs shadow-md backdrop-blur">
      {label !== undefined && (
        <div className="mb-1 font-medium text-popover-foreground">
          {labelFormatter ? labelFormatter(label) : label}
        </div>
      )}
      <div className="space-y-0.5">
        {payload.map((item, i) => (
          <div key={i} className="flex items-center gap-2 tabular-nums">
            <span className="size-2 shrink-0 rounded-sm" style={{ background: item.color }} />
            <span className="text-muted-foreground">{item.name}</span>
            <span className="ml-auto font-medium text-popover-foreground">
              {formatter && item.value !== undefined
                ? formatter(item.value, String(item.name ?? ''))
                : item.value}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}

export interface SeriesDef {
  key: string
  name: string
  color?: string
}

interface BaseChartProps {
  data: Array<Record<string, number | string>>
  xKey: string
  series: SeriesDef[]
  height?: number
  className?: string
  valueFormatter?: (value: number | string, name: string) => React.ReactNode
  labelFormatter?: (label: string | number) => React.ReactNode
  xTickFormatter?: (value: string | number) => string
  hideGrid?: boolean
  hideYAxis?: boolean
}

/** 面积趋势图(默认渐变填充) */
export function TrendAreaChart({
  data,
  xKey,
  series,
  height = 240,
  className,
  valueFormatter,
  labelFormatter,
  xTickFormatter,
  hideGrid,
  hideYAxis,
}: BaseChartProps) {
  const gradId = React.useId()
  return (
    <div className={cn('w-full', className)} style={{ height }}>
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={data} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
          <defs>
            {series.map((s, i) => (
              <linearGradient key={s.key} id={`${gradId}-${i}`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor={s.color ?? CHART_COLORS[i % CHART_COLORS.length]} stopOpacity={0.35} />
                <stop offset="100%" stopColor={s.color ?? CHART_COLORS[i % CHART_COLORS.length]} stopOpacity={0.02} />
              </linearGradient>
            ))}
          </defs>
          {!hideGrid && <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--chart-grid))" vertical={false} />}
          <XAxis dataKey={xKey} {...axisProps} tickFormatter={xTickFormatter} minTickGap={24} />
          {!hideYAxis && <YAxis {...axisProps} width={44} />}
          <Tooltip content={<ChartTooltip formatter={valueFormatter} labelFormatter={labelFormatter} />} />
          {series.map((s, i) => (
            <Area
              key={s.key}
              type="monotone"
              dataKey={s.key}
              name={s.name}
              stroke={s.color ?? CHART_COLORS[i % CHART_COLORS.length]}
              strokeWidth={2}
              fill={`url(#${gradId}-${i})`}
              dot={false}
              activeDot={{ r: 3 }}
            />
          ))}
        </AreaChart>
      </ResponsiveContainer>
    </div>
  )
}

/** 多线趋势图 */
export function TrendLineChart({
  data,
  xKey,
  series,
  height = 240,
  className,
  valueFormatter,
  labelFormatter,
  xTickFormatter,
  hideGrid,
  hideYAxis,
}: BaseChartProps) {
  return (
    <div className={cn('w-full', className)} style={{ height }}>
      <ResponsiveContainer width="100%" height="100%">
        <LineChart data={data} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
          {!hideGrid && <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--chart-grid))" vertical={false} />}
          <XAxis dataKey={xKey} {...axisProps} tickFormatter={xTickFormatter} minTickGap={24} />
          {!hideYAxis && <YAxis {...axisProps} width={44} />}
          <Tooltip content={<ChartTooltip formatter={valueFormatter} labelFormatter={labelFormatter} />} />
          {series.map((s, i) => (
            <Line
              key={s.key}
              type="monotone"
              dataKey={s.key}
              name={s.name}
              stroke={s.color ?? CHART_COLORS[i % CHART_COLORS.length]}
              strokeWidth={2}
              dot={false}
              activeDot={{ r: 3 }}
            />
          ))}
        </LineChart>
      </ResponsiveContainer>
    </div>
  )
}

/** 柱状图(支持单系列按值着色) */
export function TrendBarChart({
  data,
  xKey,
  series,
  height = 240,
  className,
  valueFormatter,
  labelFormatter,
  xTickFormatter,
  hideGrid,
  hideYAxis,
  colorByValue,
}: BaseChartProps & { colorByValue?: (entry: Record<string, number | string>) => string }) {
  return (
    <div className={cn('w-full', className)} style={{ height }}>
      <ResponsiveContainer width="100%" height="100%">
        <BarChart data={data} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
          {!hideGrid && <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--chart-grid))" vertical={false} />}
          <XAxis dataKey={xKey} {...axisProps} tickFormatter={xTickFormatter} minTickGap={8} />
          {!hideYAxis && <YAxis {...axisProps} width={44} />}
          <Tooltip
            cursor={{ fill: 'hsl(var(--muted) / 0.5)' }}
            content={<ChartTooltip formatter={valueFormatter} labelFormatter={labelFormatter} />}
          />
          {series.map((s, i) => (
            <Bar key={s.key} dataKey={s.key} name={s.name} radius={[3, 3, 0, 0]} maxBarSize={48}
              fill={s.color ?? CHART_COLORS[i % CHART_COLORS.length]}>
              {colorByValue &&
                data.map((entry, idx) => <Cell key={idx} fill={colorByValue(entry)} />)}
            </Bar>
          ))}
        </BarChart>
      </ResponsiveContainer>
    </div>
  )
}

/** 迷你火花线(指标卡内嵌,无坐标轴) */
export function Sparkline({
  data,
  dataKey,
  color = CHART_COLORS[0],
  height = 36,
  className,
}: {
  data: Array<Record<string, number | string>>
  dataKey: string
  color?: string
  height?: number
  className?: string
}) {
  const gradId = React.useId()
  return (
    <div className={cn('w-full', className)} style={{ height }}>
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={data} margin={{ top: 2, right: 0, bottom: 0, left: 0 }}>
          <defs>
            <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={color} stopOpacity={0.3} />
              <stop offset="100%" stopColor={color} stopOpacity={0} />
            </linearGradient>
          </defs>
          <Area type="monotone" dataKey={dataKey} stroke={color} strokeWidth={1.5} fill={`url(#${gradId})`} dot={false} />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  )
}

/** 环形进度(占用率/命中率等) */
export function ProgressRing({
  value,
  size = 56,
  strokeWidth = 6,
  color = 'hsl(var(--primary))',
  trackColor = 'hsl(var(--muted))',
  label,
  className,
}: {
  value: number
  size?: number
  strokeWidth?: number
  color?: string
  trackColor?: string
  label?: React.ReactNode
  className?: string
}) {
  const clamped = Math.max(0, Math.min(100, value))
  const r = (size - strokeWidth) / 2
  const circ = 2 * Math.PI * r
  const offset = circ - (clamped / 100) * circ
  return (
    <div className={cn('relative inline-flex items-center justify-center', className)} style={{ width: size, height: size }}>
      <svg width={size} height={size} className="-rotate-90">
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke={trackColor} strokeWidth={strokeWidth} />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke={color}
          strokeWidth={strokeWidth}
          strokeDasharray={circ}
          strokeDashoffset={offset}
          strokeLinecap="round"
          className="transition-all duration-500"
        />
      </svg>
      {label && <div className="absolute inset-0 flex items-center justify-center text-xs font-semibold tabular-nums">{label}</div>}
    </div>
  )
}
