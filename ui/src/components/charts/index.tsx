import * as React from 'react'
import { cn } from '@/lib/utils'

const BAR_COLORS: Record<string, string> = {
  primary: 'bg-primary',
  success: 'bg-success',
  info: 'bg-info',
  warning: 'bg-warning',
  error: 'bg-destructive',
}

interface MiniBarChartProps {
  data: number[]
  height?: number
  color?: keyof typeof BAR_COLORS
  label?: string
}

export function MiniBarChart({ data, height = 40, color = 'primary', label }: MiniBarChartProps) {
  const max = React.useMemo(() => Math.max(...data, 1), [data])
  return (
    <div className="space-y-1">
      {label && <div className="text-[0.68rem] text-muted-foreground">{label}</div>}
      <div className="flex items-end gap-0.5" style={{ height }}>
        {data.map((value, index) => (
          <div
            key={index}
            className={cn('flex-1 rounded-t-sm opacity-70 transition-all hover:opacity-100', BAR_COLORS[color])}
            style={{ height: `${Math.max((value / max) * 100, 2)}%` }}
            title={String(value)}
          />
        ))}
      </div>
    </div>
  )
}

interface SparklineProps {
  data: number[]
  height?: number
  /** CSS color value, defaults to the primary token */
  color?: string
  showArea?: boolean
}

export function Sparkline({
  data,
  height = 32,
  color = 'hsl(var(--primary))',
  showArea = true,
}: SparklineProps) {
  const { points, areaPath } = React.useMemo(() => {
    if (data.length === 0) return { points: '', areaPath: '' }
    const max = Math.max(...data, 1)
    const min = Math.min(...data, 0)
    const range = max - min || 1
    const width = 100
    const step = width / (data.length - 1 || 1)
    const coords = data.map((value, index) => {
      const x = index * step
      const y = height - ((value - min) / range) * height
      return `${x},${y}`
    })
    return {
      points: coords.join(' '),
      areaPath: `M0,${height} L${coords.join(' L')} L${width},${height} Z`,
    }
  }, [data, height])

  if (data.length === 0) {
    return (
      <div className="flex items-center justify-center text-xs text-muted-foreground/40" style={{ height }}>
        暂无数据
      </div>
    )
  }

  return (
    <svg viewBox={`0 0 100 ${height}`} className="w-full" preserveAspectRatio="none">
      {showArea && <path d={areaPath} fill={color} fillOpacity={0.1} />}
      <polyline
        points={points}
        fill="none"
        stroke={color}
        strokeWidth={1.5}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  )
}

interface ProgressRingProps {
  value: number
  max?: number
  size?: number
  strokeWidth?: number
  color?: string
  label?: string
}

export function ProgressRing({
  value,
  max = 100,
  size = 64,
  strokeWidth = 6,
  color = 'hsl(var(--primary))',
  label,
}: ProgressRingProps) {
  const radius = (size - strokeWidth) / 2
  const circumference = radius * 2 * Math.PI
  const percent = Math.min(value / max, 1)
  const offset = circumference - percent * circumference

  return (
    <div className="relative inline-flex items-center justify-center">
      <svg width={size} height={size} className="-rotate-90">
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          strokeWidth={strokeWidth}
          className="stroke-border"
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke={color}
          strokeWidth={strokeWidth}
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          strokeLinecap="round"
          className="transition-all duration-500"
        />
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center">
        <span className="text-sm font-bold">{Math.round(percent * 100)}%</span>
        {label && <span className="text-[0.6rem] text-muted-foreground">{label}</span>}
      </div>
    </div>
  )
}
