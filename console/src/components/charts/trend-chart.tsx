import { Area, AreaChart, Bar, BarChart, CartesianGrid, XAxis, YAxis } from 'recharts'
import { ChartContainer, ChartLegend, ChartLegendContent, ChartTooltip, ChartTooltipContent, type ChartConfig } from '@/components/ui/chart'
import { fmtCompact } from '@/lib/format'

export interface SeriesDef {
  key: string
  label: string
  color: string
}

/** 堆叠面积趋势图；统一 X 轴格式与 tooltip */
export function TrendChart({
  data,
  xKey,
  series,
  height = 220,
  stacked = true,
  valueFormatter = fmtCompact,
  kind = 'area',
}: {
  data: Array<Record<string, number | string>>
  xKey: string
  series: SeriesDef[]
  height?: number
  stacked?: boolean
  valueFormatter?: (v: number) => string
  kind?: 'area' | 'bar'
}) {
  const config: ChartConfig = Object.fromEntries(series.map((s) => [s.key, { label: s.label, color: s.color }]))
  const axis = (
    <>
      <CartesianGrid vertical={false} strokeDasharray="3 3" />
      <XAxis dataKey={xKey} tickLine={false} axisLine={false} tickMargin={8} minTickGap={24} fontSize={11} />
      <YAxis tickLine={false} axisLine={false} width={44} fontSize={11} tickFormatter={(v: number) => valueFormatter(v)} />
      <ChartTooltip content={<ChartTooltipContent indicator="dot" formatter={(value, name) => (
        <div className="flex w-full items-center justify-between gap-4">
          <span className="text-muted-foreground">{config[String(name)]?.label ?? name}</span>
          <span className="num font-medium">{valueFormatter(Number(value))}</span>
        </div>
      )} />} />
      {series.length > 1 && <ChartLegend content={<ChartLegendContent />} />}
    </>
  )
  return (
    <ChartContainer config={config} className="aspect-auto w-full" style={{ height }}>
      {kind === 'bar' ? (
        <BarChart data={data} margin={{ left: 0, right: 8, top: 8 }}>
          {axis}
          {series.map((s, i) => (
            <Bar
              key={s.key}
              dataKey={s.key}
              stackId={stacked ? 'a' : undefined}
              fill={`var(--color-${s.key})`}
              radius={stacked ? (i === series.length - 1 ? [3, 3, 0, 0] : 0) : [3, 3, 0, 0]}
              isAnimationActive={false}
            />
          ))}
        </BarChart>
      ) : (
        <AreaChart data={data} margin={{ left: 0, right: 8, top: 8 }}>
          <defs>
            {series.map((s) => (
              <linearGradient key={s.key} id={`fill-${s.key}`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%" stopColor={`var(--color-${s.key})`} stopOpacity={0.35} />
                <stop offset="95%" stopColor={`var(--color-${s.key})`} stopOpacity={0.02} />
              </linearGradient>
            ))}
          </defs>
          {axis}
          {series.map((s) => (
            <Area
              key={s.key}
              type="monotone"
              dataKey={s.key}
              stackId={stacked ? 'a' : undefined}
              stroke={`var(--color-${s.key})`}
              fill={`url(#fill-${s.key})`}
              strokeWidth={1.75}
              isAnimationActive={false}
            />
          ))}
        </AreaChart>
      )}
    </ChartContainer>
  )
}

/** 横向排行条：名称 + 数值 + 占比条 */
export function RankList({
  items,
  valueFormatter = fmtCompact,
  color = 'var(--chart-1)',
  footer,
  onSelect,
}: {
  items: Array<{ key: string; label: string; value: number; sub?: string }>
  valueFormatter?: (v: number) => string
  color?: string
  footer?: React.ReactNode
  onSelect?: (key: string) => void
}) {
  const max = Math.max(1, ...items.map((i) => i.value))
  return (
    <div className="space-y-2">
      <ul className="space-y-1.5">
        {items.map((i) => {
          const content = (
            <>
              <div className="flex items-baseline justify-between gap-2 text-xs">
                <span className="min-w-0 truncate" title={i.label}>
                  {i.label}
                </span>
                <span className="num shrink-0 font-medium">
                  {valueFormatter(i.value)}
                  {i.sub && <span className="ml-1.5 font-normal text-muted-foreground">{i.sub}</span>}
                </span>
              </div>
              <div className="mt-1 h-1 overflow-hidden rounded-full bg-muted">
                <div className="h-full rounded-full" style={{ width: `${(i.value / max) * 100}%`, background: color }} />
              </div>
            </>
          )
          return (
            <li key={i.key}>
              {onSelect ? (
                <button type="button" onClick={() => onSelect(i.key)} className="block w-full rounded-sm text-left hover:opacity-80">
                  {content}
                </button>
              ) : (
                content
              )}
            </li>
          )
        })}
      </ul>
      {footer}
    </div>
  )
}
