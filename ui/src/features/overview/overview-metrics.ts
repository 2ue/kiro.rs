
/** 上游原始成本 ÷ 整形后展示成本；任一为 0 时无意义。 */
export function costMultiplier(rawCostUsd: number, shapedCostUsd: number): string {
  if (!(rawCostUsd > 0) || !(shapedCostUsd > 0)) return '-'
  return (rawCostUsd / shapedCostUsd).toFixed(3)
}

/** 差额占上游原始成本的百分比。 */
export function costDeltaPercent(deltaUsd: number, rawCostUsd: number): string {
  if (!(rawCostUsd > 0)) return '-'
  return `${((deltaUsd / rawCostUsd) * 100).toFixed(2)}%`
}

export type CostDeltaTone = 'loss' | 'profit' | 'even'

/**
 * 估算计费（对下游收的钱）减去账号成本（上游原始成本 × 账号成本倍率）。
 * 账号成本为 0 时没有占比。
 */
export function accountCostDelta(reportedCostUsd: number, accountCostUsd: number): {
  deltaUsd: number
  percentText: string
  tone: CostDeltaTone
} {
  const deltaUsd = reportedCostUsd - accountCostUsd
  return {
    deltaUsd,
    percentText: accountCostUsd > 0 ? `${((deltaUsd / accountCostUsd) * 100).toFixed(1)}%` : '-',
    tone: deltaUsd < 0 ? 'loss' : deltaUsd > 0 ? 'profit' : 'even',
  }
}

export function metricsShare(part: number, total: number): number {
  return total > 0 ? part / total : 0
}

export function totalTokenUsage(metrics: {
  inputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
}): number {
  return metrics.inputTokens + metrics.outputTokens + metrics.cacheReadInputTokens + metrics.cacheCreationInputTokens
}

/** `datetime-local` 输入框的值（本地时区，到分钟）。 */
export function localDateTimeValue(value?: string | null): string {
  if (!value) return ''
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

export function localDateTimeToIso(value: string): string | undefined {
  if (!value.trim()) return undefined
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString()
}

/** 请求数加权的平均耗时；比例类数值不能对各时段直接求平均。没有请求时为 null。 */
export function weightedAverageDuration<T>(
  points: T[],
  pick: (point: T) => { requests: number; averageDurationMs?: number },
): number | null {
  const { weighted, requests } = points.reduce(
    (sum, point) => {
      const metrics = pick(point)
      return {
        weighted: sum.weighted + (metrics.averageDurationMs ?? 0) * metrics.requests,
        requests: sum.requests + metrics.requests,
      }
    },
    { weighted: 0, requests: 0 },
  )
  return requests > 0 ? weighted / requests : null
}

/** 小时桶换算成每分钟速率：数量 ÷ 60。按天粒度没有意义，调用方只在小时粒度启用。 */
export function perMinute(value: number): number {
  return value / 60
}

/** 多个小时桶的平均每分钟速率：总量 ÷ 60 ÷ 桶数；没有桶时为 null。 */
export function averagePerMinute<T>(points: T[], pick: (point: T) => number): number | null {
  if (points.length === 0) return null
  return perMinute(points.reduce((sum, point) => sum + pick(point), 0)) / points.length
}
