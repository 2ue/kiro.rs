import type { ProxyResource } from '@/types/api'

/** How the rotation list orders proxies after a full liveness test. */
export type ProxyRankMode = 'usage' | 'quality'

/** Live proxies picked automatically after ranking unless the user changes the number. */
export const DEFAULT_AUTO_PICK = 20

/** What a liveness test found for one proxy; absent when it has not been tested. */
export interface ProxyLiveness {
  success: boolean
  durationMs: number
  message?: string
}

/** How a proxy is used by accounts right now. */
export interface ProxyUsage {
  /** Bound accounts that are enabled. */
  enabled: number
  /** Bound accounts in total, enabled or not. */
  total: number
}

export interface RankedProxy {
  resource: ProxyResource
  liveness?: ProxyLiveness
  usage: ProxyUsage
  /** Live and enabled, so it can be picked for rotation. */
  usable: boolean
  /**
   * Usage tier for the usage-first order: 0 no accounts at all, 1 only disabled accounts,
   * 2 serves enabled accounts. Lower is less used.
   */
  tier: 0 | 1 | 2
}

/** Counts bound accounts per proxy resource, split by whether the account is enabled. */
export function proxyUsageById(
  credentials: Array<{ proxyResourceId?: number | null; disabled: boolean }>,
): Map<number, ProxyUsage> {
  const usage = new Map<number, ProxyUsage>()
  for (const credential of credentials) {
    const id = credential.proxyResourceId
    if (typeof id !== 'number') continue
    const current = usage.get(id) ?? { enabled: 0, total: 0 }
    current.total += 1
    if (!credential.disabled) current.enabled += 1
    usage.set(id, current)
  }
  return usage
}

function tierOf(usage: ProxyUsage): RankedProxy['tier'] {
  if (usage.total === 0) return 0
  if (usage.enabled === 0) return 1
  return 2
}

/**
 * Orders proxies for rotation. Proxies that failed the test or are disabled always sink to the
 * bottom. Usage-first puts the least used first — no accounts, then only disabled accounts, then
 * by how many enabled accounts they serve — and breaks ties by speed. Quality-first orders by
 * speed alone. Untested proxies sit after tested live ones in either order.
 */
export function rankProxies(
  resources: ProxyResource[],
  liveness: Map<number, ProxyLiveness>,
  usageById: Map<number, ProxyUsage>,
  mode: ProxyRankMode,
): RankedProxy[] {
  const ranked: RankedProxy[] = resources.map((resource) => {
    const result = liveness.get(resource.id)
    // Without an account snapshot, assume the listed bindings are all live rather than all disabled.
    const usage = usageById.get(resource.id) ?? { enabled: resource.credentialCount, total: resource.credentialCount }
    return {
      resource,
      liveness: result,
      usage,
      usable: resource.enabled && result?.success !== false,
      tier: tierOf(usage),
    }
  })
  // 0 tested and live, 1 untested, 2 failed or disabled.
  const health = (item: RankedProxy) => (!item.usable ? 2 : item.liveness ? 0 : 1)
  const speed = (item: RankedProxy) => (item.liveness?.success ? item.liveness.durationMs : Number.POSITIVE_INFINITY)
  return ranked.sort((left, right) => {
    const byHealth = health(left) - health(right)
    if (byHealth !== 0) return byHealth
    if (mode === 'usage') {
      const byTier = left.tier - right.tier
      if (byTier !== 0) return byTier
      const byLoad = left.usage.enabled - right.usage.enabled
      if (byLoad !== 0) return byLoad
    }
    const bySpeed = speed(left) - speed(right)
    if (bySpeed !== 0) return bySpeed
    return left.resource.id - right.resource.id
  })
}

/** The first `count` usable proxies of a ranking, as resource ids in rank order. */
export function autoPickProxyIds(ranked: RankedProxy[], count: number): number[] {
  return ranked.filter((item) => item.usable && item.liveness?.success).slice(0, Math.max(0, count)).map((item) => item.resource.id)
}

/**
 * Which proxy each account gets when a batch rotates over `proxyIds`: the n-th account takes
 * `proxyIds[n % len]`. Mirrors the server so a preview can show the outcome before saving.
 */
export function rotateProxyAssignments(accountIds: number[], proxyIds: number[]): Map<number, number> {
  const assignments = new Map<number, number>()
  if (proxyIds.length === 0) return assignments
  accountIds.forEach((accountId, index) => assignments.set(accountId, proxyIds[index % proxyIds.length]))
  return assignments
}
