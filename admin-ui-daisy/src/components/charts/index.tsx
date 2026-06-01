import { useMemo } from 'react'

interface MiniBarChartProps {
  data: number[]
  height?: number
  color?: string
  label?: string
}

export function MiniBarChart({ data, height = 40, color = 'primary', label }: MiniBarChartProps) {
  const max = useMemo(() => Math.max(...data, 1), [data])
  const colorClass = `bg-${color}`

  return (
    <div className="space-y-1">
      {label && <div className="text-[0.68rem] text-base-content/50">{label}</div>}
      <div className="flex items-end gap-0.5" style={{ height }}>
        {data.map((value, index) => {
          const heightPercent = (value / max) * 100
          return (
            <div
              key={index}
              className={`flex-1 rounded-t-sm ${colorClass} opacity-70 transition-all hover:opacity-100`}
              style={{ height: `${Math.max(heightPercent, 2)}%` }}
              title={String(value)}
            />
          )
        })}
      </div>
    </div>
  )
}

interface SparklineProps {
  data: number[]
  height?: number
  color?: string
  showArea?: boolean
}

export function Sparkline({ data, height = 32, color = '#3B82F6', showArea = true }: SparklineProps) {
  const points = useMemo(() => {
    if (data.length === 0) return ''
    const max = Math.max(...data, 1)
    const min = Math.min(...data, 0)
    const range = max - min || 1
    const width = 100
    const step = width / (data.length - 1 || 1)

    return data
      .map((value, index) => {
        const x = index * step
        const y = height - ((value - min) / range) * height
        return `${x},${y}`
      })
      .join(' ')
  }, [data, height])

  const areaPath = useMemo(() => {
    if (data.length === 0 || !showArea) return ''
    const max = Math.max(...data, 1)
    const min = Math.min(...data, 0)
    const range = max - min || 1
    const width = 100
    const step = width / (data.length - 1 || 1)

    const linePoints = data.map((value, index) => {
      const x = index * step
      const y = height - ((value - min) / range) * height
      return `${x},${y}`
    })

    return `M0,${height} L${linePoints.join(' L')} L${width},${height} Z`
  }, [data, height, showArea])

  if (data.length === 0) {
    return (
      <div className="flex items-center justify-center text-xs text-base-content/30" style={{ height }}>
        暂无数据
      </div>
    )
  }

  return (
    <svg viewBox={`0 0 100 ${height}`} className="w-full" preserveAspectRatio="none">
      {showArea && (
        <path d={areaPath} fill={color} fillOpacity={0.1} />
      )}
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
  color = 'primary',
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
          stroke="currentColor"
          strokeWidth={strokeWidth}
          className="text-base-300"
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="currentColor"
          strokeWidth={strokeWidth}
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          strokeLinecap="round"
          className={`text-${color} transition-all duration-500`}
        />
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center">
        <span className="text-sm font-bold">{Math.round(percent * 100)}%</span>
        {label && <span className="text-[0.6rem] text-base-content/50">{label}</span>}
      </div>
    </div>
  )
}
