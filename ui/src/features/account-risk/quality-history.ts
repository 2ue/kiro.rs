// 质量分趋势的纯逻辑。No runtime imports beyond relative `.ts` files so it can be tested with `node --test`.
import type { AccountQualityHistoryPoint } from '@/types/api'
import { formatNumber } from '../../lib/format.ts'

export type QualityChartRow = {
  /** 采样时间（毫秒时间戳），横轴的键 */
  t: number
  errorRate: number
  ttft: number
  latency: number
}

/**
 * 只保留窗口 [from, to] 内的点。新后端按 from/to 查；老后端不认 from/to、固定返回最近 24 小时，
 * 这里兜底裁掉窗口外的部分，曲线不会和所选窗口对不上。
 */
export function pointsInWindow(points: AccountQualityHistoryPoint[], from: string, to: string): AccountQualityHistoryPoint[] {
  const start = Date.parse(from)
  const end = Date.parse(to)
  if (!Number.isFinite(start) || !Number.isFinite(end)) return points
  return points.filter((point) => {
    const at = Date.parse(point.sampledAt)
    return Number.isFinite(at) && at >= start && at <= end
  })
}

export function qualityChartRows(points: AccountQualityHistoryPoint[]): QualityChartRow[] {
  return points
    .map((point) => ({
      t: Date.parse(point.sampledAt),
      errorRate: point.recentErrorRate,
      ttft: point.ttftEwmaMs ?? Number.NaN,
      latency: point.latencyEwmaMs ?? Number.NaN,
    }))
    .filter((row) => Number.isFinite(row.t))
}

function localDay(ms: number): string {
  const date = new Date(ms)
  return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`
}

/** 点分布在不同的本地日期上时，横轴刻度要带日期。 */
export function rowsCrossDays(rows: Pick<QualityChartRow, 't'>[]): boolean {
  if (rows.length < 2) return false
  return localDay(rows[0].t) !== localDay(rows[rows.length - 1].t)
}

const pad = (value: number) => String(value).padStart(2, '0')

/** 横轴刻度：同一天只写 `HH:mm`，跨天写 `MM/DD HH:mm`。 */
export function formatQualityTick(ms: number | string, withDate: boolean): string {
  const date = new Date(Number(ms))
  if (Number.isNaN(date.getTime())) return ''
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`
  return withDate ? `${pad(date.getMonth() + 1)}/${pad(date.getDate())} ${time}` : time
}

/** 提示框里的完整时间：`MM/DD HH:mm`。 */
export function formatQualityLabel(ms: number | string): string {
  return formatQualityTick(ms, true)
}

/** 失败率纵轴：百分比，最多一位小数并去掉多余的 0（0%、2.5%、10%）。 */
export function formatRateTick(value: number | string): string {
  const number = Number(value)
  if (!Number.isFinite(number)) return ''
  return `${(number * 100).toFixed(1).replace(/\.0$/, '')}%`
}

/** 延迟纵轴：毫秒取整，带千分位。 */
export function formatMsTick(value: number | string): string {
  const number = Number(value)
  if (!Number.isFinite(number)) return ''
  return `${formatNumber(Math.round(number))} ms`
}

/** 降采样桶宽的说明；一分钟一个点（未降采样）或老后端没给时返回空串。 */
export function bucketDescription(bucketSecs: number | null | undefined): string {
  if (!bucketSecs || !Number.isFinite(bucketSecs) || bucketSecs <= 60) return ''
  if (bucketSecs % 3600 === 0) return `每 ${bucketSecs / 3600} 小时一个点（桶内平均）`
  if (bucketSecs % 60 === 0) return `每 ${bucketSecs / 60} 分钟一个点（桶内平均）`
  return `每 ${bucketSecs} 秒一个点（桶内平均）`
}
