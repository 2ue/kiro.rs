import { describe, expect, it } from 'vitest'
import type { CredentialAccountInfo } from '@/api/types'
import { deriveQuota, quotaLevel } from '../quota'

const info = (o: Partial<CredentialAccountInfo>): CredentialAccountInfo => ({
  subscriptionTitle: 'KIRO PRO',
  currentUsage: 0,
  usageLimit: 1000,
  remaining: 1000,
  usagePercentage: 0,
  creditLimit: 0,
  creditRemaining: 0,
  creditBase: 0,
  creditBonus: 0,
  overageCap: 0,
  overageRate: 0,
  currentOverages: 0,
  nextResetAt: null,
  checkedAt: new Date().toISOString(),
  ...o,
})

describe('quota', () => {
  it('levels', () => {
    expect(quotaLevel(0.1)).toBe('ok')
    expect(quotaLevel(0.5)).toBe('warn')
    expect(quotaLevel(0.85)).toBe('high')
    expect(quotaLevel(1.01)).toBe('over')
  })

  it('unknown when no snapshot (never shows 0%)', () => {
    expect(deriveQuota(undefined).level).toBe('unknown')
    expect(deriveQuota(info({ usageLimit: 0 })).level).toBe('unknown')
  })

  it('computes ratio and reset date', () => {
    const q = deriveQuota(info({ currentUsage: 900, nextResetAt: 1_900_000_000 }))
    expect(q.ratio).toBeCloseTo(0.9)
    expect(q.level).toBe('high')
    expect(q.nextResetAt?.getTime()).toBe(1_900_000_000_000)
  })

  it('stale after 30 minutes', () => {
    const now = Date.now()
    expect(deriveQuota(info({ checkedAt: new Date(now - 31 * 60e3).toISOString() }), now).stale).toBe(true)
    expect(deriveQuota(info({ checkedAt: new Date(now - 5 * 60e3).toISOString() }), now).stale).toBe(false)
  })

  it('overage flags', () => {
    const q = deriveQuota(info({ overageStatus: 'ENABLED', currentOverages: 1.5 }))
    expect(q.overageEnabled).toBe(true)
    expect(q.overageUsd).toBe(1.5)
  })
})
