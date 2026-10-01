import { describe, expect, it } from 'vitest'
import { deriveCredentialStatus } from '../credential-status'

const base = {
  disabled: false,
  disabledReason: undefined,
  cooledDown: false,
  cooldownRemainingSecs: 0,
  cooldownReason: undefined,
  cooldowns: [],
  rateLimited: false,
  rateLimitRemainingSecs: 0,
  inFlightRequests: 0,
  maxConcurrentRequests: 4,
  inProbation: false,
  probationRemainingSecs: 0,
  warmupRemaining: 0,
  failureCount: 0,
  refreshFailureCount: 0,
  lastErrorKind: undefined,
  lastErrorReason: undefined,
  recentErrorRate: 0,
}

describe('deriveCredentialStatus', () => {
  it('healthy by default', () => {
    const v = deriveCredentialStatus(base)
    expect(v.primary).toBe('healthy')
    expect(v.schedulable).toBe(true)
  })

  it('disabled account is never schedulable, even if otherwise idle', () => {
    const v = deriveCredentialStatus({ ...base, disabled: true, disabledReason: 'QuotaExceeded' })
    expect(v.primary).toBe('disabled')
    expect(v.schedulable).toBe(false)
    expect(v.disabledReason?.label).toBe('额度用尽')
    expect(v.disabledReason?.action).toBe('wait_reset')
  })

  it('missing disabled reason falls back to manual', () => {
    const v = deriveCredentialStatus({ ...base, disabled: true })
    expect(v.disabledReason?.code).toBe('Manual')
  })

  it('unknown disabled reason keeps raw code', () => {
    const v = deriveCredentialStatus({ ...base, disabled: true, disabledReason: 'SomethingNew' })
    expect(v.disabledReason?.label).toBe('SomethingNew')
  })

  it('priority: disabled > rateLimited > cooling > saturated > failing', () => {
    const all = { ...base, rateLimited: true, rateLimitRemainingSecs: 30, cooledDown: true, cooldownRemainingSecs: 10, inFlightRequests: 4, failureCount: 2 }
    expect(deriveCredentialStatus({ ...all, disabled: true }).primary).toBe('disabled')
    const rl = deriveCredentialStatus(all)
    expect(rl.primary).toBe('rateLimited')
    expect(rl.remainingSecs).toBe(30)
    expect(rl.secondary).toEqual(['cooling', 'saturated', 'failing', 'busy'])
    expect(deriveCredentialStatus({ ...all, rateLimited: false }).primary).toBe('cooling')
    expect(deriveCredentialStatus({ ...all, rateLimited: false, cooledDown: false }).primary).toBe('saturated')
  })

  it('global cooldown entry blocks scheduling; model-only cooldown does not', () => {
    const global = deriveCredentialStatus({ ...base, cooldowns: [{ global: true, remainingSecs: 5 }] })
    expect(global.schedulable).toBe(false)
    const model = deriveCredentialStatus({ ...base, cooldowns: [{ global: false, model: 'x', remainingSecs: 5 }] })
    expect(model.schedulable).toBe(true)
    expect(model.primary).toBe('healthy')
  })

  it('failing stays schedulable', () => {
    const v = deriveCredentialStatus({ ...base, recentErrorRate: 0.6, lastErrorReason: 'boom' })
    expect(v.primary).toBe('failing')
    expect(v.schedulable).toBe(true)
    expect(v.detail).toBe('boom')
  })

  it('unlimited concurrency never saturates', () => {
    expect(deriveCredentialStatus({ ...base, maxConcurrentRequests: 0, inFlightRequests: 99 }).primary).toBe('busy')
  })
})
