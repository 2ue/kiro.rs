import { formatCompact, formatNumber } from '../../lib/format.ts'

interface CacheWriteSplit {
  cacheCreationInputTokens: number
  cacheCreation5mInputTokens?: number
  cacheCreation1hInputTokens?: number
}

/**
 * The 1-hour marker after a cache-write count. Only writes carry a TTL (reads have none), so the
 * marker appears only when some of the write went to the 1-hour cache: `1h` when all of it did,
 * `1h 10K` for a mix.
 */
export function cacheWriteTtlMarker(split: CacheWriteSplit): string | undefined {
  const oneHour = split.cacheCreation1hInputTokens ?? 0
  if (!(oneHour > 0)) return undefined
  return oneHour >= split.cacheCreationInputTokens ? '1h' : `1h ${formatCompact(oneHour)}`
}

/** Exact 5-minute / 1-hour split for a tooltip; undefined when nothing was written. */
export function cacheWriteTtlTitle(split: CacheWriteSplit): string | undefined {
  if (!(split.cacheCreationInputTokens > 0)) return undefined
  const oneHour = split.cacheCreation1hInputTokens ?? 0
  const fiveMinutes = split.cacheCreation5mInputTokens ?? Math.max(0, split.cacheCreationInputTokens - oneHour)
  return `缓存写入 ${formatNumber(split.cacheCreationInputTokens)}：5 分钟 ${formatNumber(fiveMinutes)} · 1 小时 ${formatNumber(oneHour)}`
}
