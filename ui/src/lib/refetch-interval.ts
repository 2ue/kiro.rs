// No runtime imports so it can be tested with `node --test`.

/** 扫用量明细的重查询（延迟分位等）：缓存至少保留这么久，自动刷新也不快于它。 */
export const HEAVY_QUERY_MIN_INTERVAL_MS = 60_000

/** 自动刷新间隔不短于 `minMs`；关闭（false）或非正数保持关闭。 */
export function atLeastInterval(interval: number | false, minMs = HEAVY_QUERY_MIN_INTERVAL_MS): number | false {
  if (interval === false || !Number.isFinite(interval) || interval <= 0) return false
  return Math.max(interval, minMs)
}
