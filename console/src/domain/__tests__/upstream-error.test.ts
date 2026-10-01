import { describe, expect, it } from 'vitest'
import { translateError } from '../upstream-error'

describe('translateError', () => {
  it('token refresh kind', () => {
    const t = translateError(
      '内部错误: token refresh failed: stage=validation kind=invalid_configuration status=none retry_after_ms=none send_committed=false',
    )
    expect(t?.title).toBe('凭据配置不完整')
    expect(t?.raw).toContain('kind=invalid_configuration')
  })
  it('known upstream codes', () => {
    expect(translateError('400 Bad Request: {"reason":"MONTHLY_REQUEST_COUNT"}')?.title).toBe('账号额度已用尽')
    expect(translateError('ThrottlingException: slow down')?.title).toBe('上游限流')
    expect(translateError('error sending request: operation timed out')?.title).toBe('请求超时')
  })
  it('unknown keeps text', () => {
    expect(translateError('something odd')?.title).toBe('something odd')
    expect(translateError('')).toBeNull()
  })
})
