import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { useMemo } from 'react'
import { credentialsApi } from '@/api/endpoints/credentials'
import type { CredentialStatusItem, CredentialsPageQuery } from '@/api/types'
import { deriveCredentialStatus, type CredentialStatusView } from '@/domain/credential-status'
import { deriveQuota, type QuotaView } from '@/domain/quota'
import { qk } from '@/queries/keys'
import { usePollInterval } from '@/stores/ui-prefs'
import type { AccountsSearch } from './search'

export interface AccountRow extends CredentialStatusItem {
  status: CredentialStatusView
  quota: QuotaView
  label: string
}

export function credentialLabel(c: Pick<CredentialStatusItem, 'id' | 'email' | 'maskedApiKey'>): string {
  return c.email || c.maskedApiKey || `账号 #${c.id}`
}

export function toAccountRow(c: CredentialStatusItem): AccountRow {
  return { ...c, status: deriveCredentialStatus(c), quota: deriveQuota(c.accountInfo), label: credentialLabel(c) }
}

export function searchToQuery(s: AccountsSearch): CredentialsPageQuery {
  const status = s.status && s.status !== 'all' ? s.status : undefined
  const q = s.q?.trim()
  const asId = q && /^#?\d+$/.test(q) ? Number(q.replace('#', '')) : undefined
  return {
    page: s.page ?? 1,
    limit: s.size ?? 50,
    q: asId ? undefined : q || undefined,
    credentialId: asId,
    status,
    authMethod: s.auth,
    subscription: s.sub,
    region: s.region,
    proxyResourceId: s.proxy,
    model: s.model,
    sortBy: s.sort ?? 'default',
    sortOrder: s.order ?? 'desc',
  }
}

/**
 * 账号分页：/credentials-paged 一次返回基础字段 + 运行态 + 额度快照 + 成本，
 * 每行不再单独发请求。禁用原因筛选目前在前端对当前页生效。
 */
export function useAccountsPage(search: AccountsSearch) {
  const query = searchToQuery(search)
  const interval = usePollInterval('fast')
  const result = useQuery({
    queryKey: qk.credentials.page(query),
    queryFn: () => credentialsApi.page(query),
    placeholderData: keepPreviousData,
    refetchInterval: interval,
  })
  const rows = useMemo(() => {
    const all = (result.data?.credentials ?? []).map(toAccountRow)
    if (search.status === 'disabled' && search.reason) {
      return all.filter((r) => (r.disabledReason ?? 'Manual') === search.reason)
    }
    return all
  }, [result.data, search.status, search.reason])
  return { ...result, rows }
}

/** 单个账号：优先复用列表缓存，缺失时按 ID 查询 */
export function useAccount(id: number | undefined) {
  const queryClient = useQueryClient()
  const interval = usePollInterval('fast')
  return useQuery({
    queryKey: qk.credentials.detail(id ?? -1),
    enabled: typeof id === 'number',
    refetchInterval: interval,
    queryFn: async () => {
      const res = await credentialsApi.page({ page: 1, limit: 1, credentialId: id })
      const item = res.credentials.find((c) => c.id === id)
      if (!item) throw new Error(`账号 #${id} 不存在或已被删除`)
      return toAccountRow(item)
    },
    initialData: () => {
      const pages = queryClient.getQueriesData<{ credentials: CredentialStatusItem[] }>({ queryKey: ['credentials', 'page'] })
      for (const [, data] of pages) {
        const hit = data?.credentials.find((c) => c.id === id)
        if (hit) return toAccountRow(hit)
      }
      return undefined
    },
    initialDataUpdatedAt: 0,
  })
}

export function useCreditSummary() {
  return useQuery({
    queryKey: qk.credentials.creditSummary,
    queryFn: credentialsApi.creditSummary,
    refetchInterval: usePollInterval('slow'),
  })
}

export function useDiagnostics(id: number, page: number) {
  return useQuery({
    queryKey: qk.credentials.diagnostics(id, page),
    queryFn: () => credentialsApi.diagnostics(id, page, 20),
    placeholderData: keepPreviousData,
  })
}
