import type { CredentialsPageQuery, UsageRecordsPageQuery } from '@/api/types'

/** 所有 query key 的唯一来源；失效时按领域前缀匹配 */
export const qk = {
  credentials: {
    all: ['credentials'] as const,
    page: (q: CredentialsPageQuery) => ['credentials', 'page', q] as const,
    summary: ['credentials', 'summary'] as const,
    creditSummary: ['credentials', 'credit-summary'] as const,
    runtime: (ids: number[]) => ['credentials', 'runtime', ids] as const,
    diagnostics: (id: number, page: number) => ['credentials', 'diagnostics', id, page] as const,
    detail: (id: number) => ['credentials', 'detail', id] as const,
  },
  pools: {
    all: ['pools'] as const,
    list: ['pools', 'list'] as const,
    status: ['pools', 'status'] as const,
  },
  proxies: {
    all: ['proxies'] as const,
    list: ['proxies', 'list'] as const,
  },
  usage: {
    all: ['usage'] as const,
    records: (q: UsageRecordsPageQuery) => ['usage', 'records', q] as const,
    summary: ['usage', 'summary'] as const,
    writer: ['usage', 'writer'] as const,
    windows: ['usage', 'windows'] as const,
    series: ['usage', 'series'] as const,
    top: (w: string) => ['usage', 'top', w] as const,
    breakdown: (w: string) => ['usage', 'breakdown', w] as const,
    accounts: (params: object) => ['usage', 'accounts', params] as const,
    poolBilling: (w: string) => ['usage', 'pool-billing', w] as const,
    poolRisk: (q: object) => ['usage', 'pool-risk', q] as const,
    cleanup: ['usage', 'cleanup'] as const,
  },
  system: {
    version: ['system', 'version'] as const,
    loadBalancing: ['system', 'load-balancing'] as const,
    runtimeConfig: ['system', 'runtime-config'] as const,
    accessKeys: ['system', 'access-keys'] as const,
    audit: (page: number, limit: number) => ['system', 'audit', page, limit] as const,
    pricing: ['system', 'pricing'] as const,
    capabilities: ['system', 'capabilities'] as const,
  },
}
