import type { Account, AccountStatus } from '@/types/api'

/**
 * 账号/账号列表的排序分层：正在使用的排最前，其次可用，再次冷却，最后不可用。
 * 只用“是否在用”分层，不按在途数量排序，避免每次刷新时行顺序来回跳动；
 * 同一层内沿用原来的消耗排序。
 */
export type UsageTier = 'active' | 'ready' | 'cooling' | 'unavailable'

export const USAGE_TIER_RANK: Record<UsageTier, number> = {
  active: 0,
  ready: 1,
  cooling: 2,
  unavailable: 3,
}

export function accountUsageTier(
  pool: Pick<Account, 'enabled' | 'autoDisabled'> | undefined,
  status: Pick<AccountStatus, 'inFlight' | 'cooldownRemainingSecs'> | undefined,
): UsageTier {
  // 已删除但还有历史用量的池：没有配置，只能算不可用。
  if (!pool || !pool.enabled || pool.autoDisabled) return 'unavailable'
  if ((status?.inFlight ?? 0) > 0) return 'active'
  if ((status?.cooldownRemainingSecs ?? 0) > 0) return 'cooling'
  return 'ready'
}

export function compareUsageTier(left: UsageTier, right: UsageTier): number {
  return USAGE_TIER_RANK[left] - USAGE_TIER_RANK[right]
}
