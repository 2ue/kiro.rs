import { describe, expect, it } from 'vitest'
import { validateConfig } from '@/features/settings/validate'

describe('validateConfig', () => {
  it('tolerates partial config from older backends', () => {
    expect(validateConfig({ streamKeepaliveIntervalSecs: 5 })).toEqual([])
  })
  it('catches cross-field errors', () => {
    expect(validateConfig({ credentialMaxCooldownSecs: 10, credentialRateLimitCooldownSecs: 20 })).toHaveLength(1)
    expect(validateConfig({ payloadGuardMaxBytes: 1000 }).length).toBeGreaterThan(0)
    expect(validateConfig({ definedCacheRoutes: ['/bad'] })).toHaveLength(1)
  })
})
