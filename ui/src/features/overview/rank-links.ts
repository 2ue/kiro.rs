// 总览排行点进用量页的筛选条件。No runtime imports beyond relative `.ts` files so it can be tested with `node --test`.
import { normalizeRequestApiKeyId } from '../../lib/request-api-key-id.ts'
import { routePrefixForEndpoint, UNKNOWN_USAGE_ROUTE } from '../routes/route-utils.ts'

export type RankingDimension = 'models' | 'accounts' | 'keys' | 'paths' | 'errors'

/** 带到用量页的筛选键（时间范围由调用方补）。 */
export type RankUsageFilter = {
  accountId?: number
  /** 入口路径子串 */
  endpoint?: string
  /** 路由前缀，默认路由为 "/" */
  route?: string
  model?: string
  requestApiKeyId?: string
}

/**
 * 排行一行点进用量页时的筛选；返回 null 表示这一行不可点：
 * 错误页签按错误类型聚合没有对应筛选，任何维度里 key 为 `unknown` 的行也没有可用的筛选值。
 * 路由页签的行是入口路径：按路径推出的路由前缀筛（不会混进其它路由），再用路径缩到这条入口；
 * 不含 `/v1` 的路径推不出路由，只按路径筛。
 */
export function rankUsageFilter(dimension: RankingDimension, key: string): RankUsageFilter | null {
  const value = key.trim()
  if (!value || value === UNKNOWN_USAGE_ROUTE) return null
  switch (dimension) {
    case 'models':
      return { model: value }
    case 'paths': {
      const route = routePrefixForEndpoint(value)
      if (route === UNKNOWN_USAGE_ROUTE) return null
      return route === value ? { endpoint: value } : { route, endpoint: value }
    }
    case 'keys': {
      const requestApiKeyId = normalizeRequestApiKeyId(value)
      return requestApiKeyId ? { requestApiKeyId } : null
    }
    case 'accounts': {
      const accountId = Number(value)
      return Number.isInteger(accountId) && accountId > 0 ? { accountId } : null
    }
    default:
      return null
  }
}
