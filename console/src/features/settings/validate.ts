import type { RuntimeConfig } from '@/api/types'

const DFCACHE = /^\/dfcache\/[A-Za-z0-9._-]+$/

/**
 * 与后端校验保持一致的跨字段检查；返回错误列表，空表示通过。
 * 旧版本后端可能缺少部分字段，缺失时跳过对应检查而不是报错。
 */
export function validateConfig(c: Partial<RuntimeConfig>): string[] {
  const errors: string[] = []
  const max = c.credentialMaxCooldownSecs
  if (typeof max === 'number') {
    if ((c.credentialTransientCooldownSecs ?? 0) > max) errors.push('其他临时错误冷却不能大于最大冷却时长')
    const perType = [
      c.credentialRateLimitCooldownSecs,
      c.credentialServerErrorCooldownSecs,
      c.credentialNetworkErrorCooldownSecs,
      c.credentialStreamErrorCooldownSecs,
      c.credentialProtocolErrorCooldownSecs,
      c.credentialAuthErrorCooldownSecs,
    ]
    if (perType.some((v) => (v ?? 0) > max)) errors.push('按错误类型的冷却时间不能大于最大冷却时长')
  }
  if ((c.promptCacheCapJitterMinTokens ?? 0) > (c.promptCacheCapJitterMaxTokens ?? Infinity)) errors.push('触顶扣减下限不能大于上限')
  const guard = c.payloadGuardMaxBytes ?? 0
  if (guard > 0 && guard < 65536) errors.push('请求大小阈值必须为 0 或不小于 65536 字节')
  if (guard > 0 && guard - (c.payloadGuardSafetyMarginBytes ?? 0) < 65536) errors.push('请求大小阈值减去安全余量需不小于 65536 字节')
  const mt = c.missingMaxTokens?.defaultValue
  if (typeof mt === 'number' && (mt < 1 || mt > 200000)) errors.push('max_tokens 补充值必须在 1 到 200000 之间')
  const bad = (c.definedCacheRoutes ?? []).find((r) => r.trim() && !DFCACHE.test(r.trim()))
  if (bad) errors.push(`自定义缓存路径 ${bad} 无效：必须是 /dfcache/{name}`)
  const ep = c.externalPools
  if (
    ep &&
    typeof ep.externalPoolMaxProbationSecs === 'number' &&
    ep.externalPoolMaxProbationSecs < (ep.externalPoolDegradeProbationSecs ?? 0)
  )
    errors.push('外部池最长降级时长不能低于单次降级时长')
  return errors
}
