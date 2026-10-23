/**
 * 缓存命中率，与 sub2api 口径一致：
 *   缓存读 / (未缓存输入 + 缓存读 + 缓存写)
 * 按 token 加权：任何维度都先把三类 token 求和再相除，不对比率取平均。
 */
export type CacheTokenMetrics = {
  uncachedInputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
}

/** 分母为 0（该时段没有输入）时返回 null，图表断开、表格显示“-”。 */
export function cacheHitRate(metrics: CacheTokenMetrics): number | null {
  const denominator = metrics.uncachedInputTokens + metrics.cacheReadInputTokens + metrics.cacheCreationInputTokens
  return denominator > 0 ? metrics.cacheReadInputTokens / denominator : null
}

export function sumCacheTokens(items: CacheTokenMetrics[]): CacheTokenMetrics {
  return items.reduce<CacheTokenMetrics>(
    (sum, item) => ({
      uncachedInputTokens: sum.uncachedInputTokens + item.uncachedInputTokens,
      cacheReadInputTokens: sum.cacheReadInputTokens + item.cacheReadInputTokens,
      cacheCreationInputTokens: sum.cacheCreationInputTokens + item.cacheCreationInputTokens,
    }),
    { uncachedInputTokens: 0, cacheReadInputTokens: 0, cacheCreationInputTokens: 0 },
  )
}

// sub2api 渠道监控的默认阈值：≥85% 正常，60%~85% 预警，<60% 严重。
export const CACHE_HIT_RATE_WARNING = 0.85
export const CACHE_HIT_RATE_CRITICAL = 0.6

export type CacheHitRateLevel = 'ok' | 'warning' | 'critical' | 'none'

export function cacheHitRateLevel(rate: number | null): CacheHitRateLevel {
  if (rate == null) return 'none'
  if (rate < CACHE_HIT_RATE_CRITICAL) return 'critical'
  if (rate < CACHE_HIT_RATE_WARNING) return 'warning'
  return 'ok'
}
