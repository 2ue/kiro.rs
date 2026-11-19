
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
 * 金额差额按两位小数展示：绝对值不到半分（0.005）的浮点残差按 0 处理，
 * 避免显示成红色的 "-$0.00"。
 */
export function zeroBelowCent(value: number): number {
  return Number.isFinite(value) && Math.abs(value) >= 0.005 ? value : 0
}

/**
 * 估算计费（对下游收的钱）减去账号成本（上游原始成本 × 账号成本倍率）。
 * 账号成本为 0 时没有占比；不到半分的差额按 0。
 */
export function accountCostDelta(reportedCostUsd: number, accountCostUsd: number): {
  deltaUsd: number
  percentText: string
  tone: CostDeltaTone
} {
  const deltaUsd = zeroBelowCent(reportedCostUsd - accountCostUsd)
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

/**
 * 已经结束的时段：`partialFrom` 起是还没结束的桶（当前小时或当天）。
 * 平均速率的分子分母都只用已结束的桶，进行中的那一小时会把平均值拉低。
 */
export function completedBuckets<T>(points: T[], partialFrom: number): T[] {
  return points.slice(0, Math.max(0, Math.min(points.length, partialFrom)))
}

const HOUR_MS = 3_600_000

/**
 * 延迟分位接口的范围上限判断：后端先把 from 向下、to 向上对齐到整点（UTC）再比较跨度，
 * 前端按同一口径判断，免得恰好 8 天但不是整点的范围被前端放行、后端拒绝。
 */
export function latencyRangeWithinLimit(fromIso: string, toIso: string, maxDays = 8): boolean {
  const from = Date.parse(fromIso)
  const to = Date.parse(toIso)
  if (!Number.isFinite(from) || !Number.isFinite(to)) return false
  const alignedFrom = Math.floor(from / HOUR_MS) * HOUR_MS
  const alignedTo = Math.ceil(to / HOUR_MS) * HOUR_MS
  return alignedTo - alignedFrom <= maxDays * 86_400_000
}
