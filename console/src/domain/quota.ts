import type { CredentialAccountInfo } from '@/api/types'

export type QuotaLevel = 'ok' | 'warn' | 'high' | 'over' | 'unknown'

/** 快照超过该时长视为可能过期 */
export const QUOTA_STALE_MS = 30 * 60 * 1000

export interface QuotaView {
  level: QuotaLevel
  used: number
  limit: number
  /** 0~1+；超额时 >1 */
  ratio: number
  remaining: number
  nextResetAt: Date | null
  checkedAt: Date | null
  stale: boolean
  /** 已开启超额（overageStatus=ENABLED） */
  overageEnabled: boolean
  /** 本周期已产生的超额费用（USD） */
  overageUsd: number
}

export function quotaLevel(ratio: number): QuotaLevel {
  if (ratio > 1) return 'over'
  if (ratio >= 0.8) return 'high'
  if (ratio >= 0.5) return 'warn'
  return 'ok'
}

export function deriveQuota(info: CredentialAccountInfo | null | undefined, now = Date.now()): QuotaView {
  if (!info || !(info.usageLimit > 0)) {
    return {
      level: 'unknown',
      used: info?.currentUsage ?? 0,
      limit: info?.usageLimit ?? 0,
      ratio: 0,
      remaining: info?.remaining ?? 0,
      nextResetAt: info?.nextResetAt ? new Date(info.nextResetAt * 1000) : null,
      checkedAt: info?.checkedAt ? new Date(info.checkedAt) : null,
      stale: true,
      overageEnabled: (info?.overageStatus ?? '').toUpperCase() === 'ENABLED',
      overageUsd: info?.currentOverages ?? 0,
    }
  }
  const used = info.currentUsage
  const ratio = used / info.usageLimit
  const checkedAt = info.checkedAt ? new Date(info.checkedAt) : null
  return {
    level: quotaLevel(ratio),
    used,
    limit: info.usageLimit,
    ratio,
    remaining: Math.max(0, info.remaining),
    nextResetAt: info.nextResetAt ? new Date(info.nextResetAt * 1000) : null,
    checkedAt,
    stale: !checkedAt || now - checkedAt.getTime() > QUOTA_STALE_MS,
    overageEnabled: (info.overageStatus ?? '').toUpperCase() === 'ENABLED',
    overageUsd: info.currentOverages ?? 0,
  }
}
