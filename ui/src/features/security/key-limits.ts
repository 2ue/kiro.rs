// Per-key usage limit helpers. No runtime imports so they can be tested with `node --test`.
import type { KeyLimitMetric, KeyLimitPeriod, KeyLimitStatus, KeyUsageLimit } from '@/types/api'

export const METRIC_OPTIONS: { value: KeyLimitMetric; label: string; unit: string }[] = [
  { value: 'cost_usd', label: '计费金额', unit: 'USD' },
  { value: 'credits', label: '积分', unit: '积分' },
  { value: 'tokens', label: 'Token', unit: 'Token' },
]

export const PERIOD_OPTIONS: { value: KeyLimitPeriod; label: string }[] = [
  { value: 'daily', label: '每天' },
  { value: 'monthly', label: '每月' },
  { value: 'total', label: '累计' },
]

export const MAX_LIMITS_PER_KEY = 9

export function metricLabel(metric: KeyLimitMetric): string {
  return METRIC_OPTIONS.find((item) => item.value === metric)?.label ?? metric
}

export function periodLabel(period: KeyLimitPeriod): string {
  return PERIOD_OPTIONS.find((item) => item.value === period)?.label ?? period
}

/** Amounts with at most 6 decimals; tokens as grouped integers. */
export function formatLimitValue(metric: KeyLimitMetric, value: number): string {
  if (!Number.isFinite(value)) return '-'
  const digits = metric === 'tokens' ? 0 : 6
  const text = new Intl.NumberFormat('zh-CN', { minimumFractionDigits: 0, maximumFractionDigits: digits, useGrouping: true }).format(value)
  return metric === 'cost_usd' ? `$${text}` : text
}

/** Short label of one limit, e.g. "每天 计费金额 $5". */
export function limitSummary(limit: KeyUsageLimit): string {
  return `${periodLabel(limit.period)} ${metricLabel(limit.metric)} ${formatLimitValue(limit.metric, limit.limit)}`
}

/** One editable row in the key dialog. `value` keeps the raw text so typing isn't fought. */
export interface LimitDraft {
  metric: KeyLimitMetric
  period: KeyLimitPeriod
  value: string
}

export function toDrafts(limits: KeyUsageLimit[] | undefined): LimitDraft[] {
  return (limits ?? []).map((limit) => ({ metric: limit.metric, period: limit.period, value: String(limit.limit) }))
}

/** First metric + period pair not used yet, for "add limit". */
export function nextDraft(drafts: LimitDraft[]): LimitDraft | null {
  for (const period of PERIOD_OPTIONS) {
    for (const metric of METRIC_OPTIONS) {
      if (!drafts.some((draft) => draft.metric === metric.value && draft.period === period.value)) {
        return { metric: metric.value, period: period.value, value: '' }
      }
    }
  }
  return null
}

/** Validates the rows the same way the server does; returns limits or the first error. */
export function parseDrafts(drafts: LimitDraft[]): { limits: KeyUsageLimit[]; error: string | null } {
  if (drafts.length > MAX_LIMITS_PER_KEY) return { limits: [], error: `每个 Key 最多设置 ${MAX_LIMITS_PER_KEY} 条用量限制` }
  const seen = new Set<string>()
  const limits: KeyUsageLimit[] = []
  for (const draft of drafts) {
    const name = `${periodLabel(draft.period)}${metricLabel(draft.metric)}`
    const limit = Number(draft.value.trim())
    if (!draft.value.trim() || !Number.isFinite(limit) || limit <= 0) return { limits: [], error: `${name}限制必须是大于 0 的数字` }
    if (draft.metric === 'tokens' && !Number.isInteger(limit)) return { limits: [], error: 'Token 限制必须是整数' }
    const key = `${draft.metric}:${draft.period}`
    if (seen.has(key)) return { limits: [], error: `${name}限制重复` }
    seen.add(key)
    limits.push({ metric: draft.metric, period: draft.period, limit })
  }
  return { limits, error: null }
}

export type UsageTone = 'success' | 'warning' | 'error'

/** Share used (0-100, capped) and tone: amber from 80%, red once the limit is reached. */
export function usageView(status: KeyLimitStatus): { percent: number; tone: UsageTone } {
  const percent = status.limit > 0 ? Math.min(100, Math.max(0, (status.used / status.limit) * 100)) : 0
  const tone: UsageTone = status.exceeded ? 'error' : percent >= 80 ? 'warning' : 'success'
  return { percent, tone }
}
