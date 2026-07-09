import type {
  BalanceResponse,
  CredentialAccountInfo,
  CredentialListItem,
  CredentialRuntimeItem,
  CredentialStatusItem,
  CredentialUsageSummaryItem,
} from '@/types/api'

// ============================================================================
// Label helpers
// ============================================================================

export function credentialLabel(c: Pick<CredentialStatusItem, 'id' | 'email' | 'maskedApiKey'>) {
  return c.email || c.maskedApiKey || `账号 #${c.id}`
}

export function authLabel(authMethod: string | null | undefined) {
  if (authMethod === 'api_key') return 'API Key'
  if (authMethod === 'idc') return 'IdC'
  if (authMethod === 'external_idp') return 'External IdP'
  if (authMethod === 'social') return 'Social'
  return authMethod || 'Unknown'
}

type BadgeTone = 'neutral' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' | 'info'

export function subscriptionBadgeMeta(
  cred: Pick<CredentialStatusItem, 'subscriptionTitle' | 'accountInfo'>,
  balance?: BalanceResponse
): { label: string; tone: BadgeTone; title?: string } {
  const raw = balance?.subscriptionTitle || cred.accountInfo?.subscriptionTitle || cred.subscriptionTitle || ''
  if (!raw) return { label: '未知套餐', tone: 'secondary' }
  const normalized = raw.toLowerCase().replace(/[_\s-]+/g, ' ')
  if (normalized.includes('power')) return { label: 'Power', tone: 'primary', title: raw }
  if (normalized.includes('pro plus') || normalized.includes('pro+')) return { label: 'Pro+', tone: 'primary', title: raw }
  if (normalized.includes('pro')) return { label: 'Pro', tone: 'primary', title: raw }
  if (normalized.includes('free')) return { label: 'Free', tone: 'secondary', title: raw }
  if (normalized.includes('trial') || normalized.includes('试用')) return { label: 'Trial', tone: 'info', title: raw }
  return { label: raw.length > 12 ? raw.slice(0, 12) + '…' : raw, tone: 'neutral', title: raw }
}

export function endpointLabel(endpoint?: string | null): string {
  if (!endpoint) return ''
  const v = endpoint.trim()
  if (!v) return ''
  const lower = v.toLowerCase()
  if (lower === 'ide') return 'IDE'
  if (lower === 'idc') return 'IDC'
  if (lower === 'api_key') return 'API Key'
  if (lower.includes('power')) return 'Power'
  return v.replace(/_/g, ' ').toUpperCase().slice(0, 10)
}

export function sourceLabel(src?: CredentialStatusItem['effectiveProxySource']): string {
  const labels: Record<string, string> = {
    credential: '直接代理',
    resource: '代理资源',
    resource_disabled: '代理已禁用',
    resource_missing: '代理不存在',
    global: '全局代理',
    direct: '直连',
    none: '无代理',
  }
  return labels[src || ''] || '未配置'
}

export function proxySummary(c: Pick<CredentialStatusItem, 'effectiveProxySource' | 'proxyResourceName'>): string {
  const label = sourceLabel(c.effectiveProxySource)
  if (
    c.proxyResourceName &&
    (c.effectiveProxySource === 'resource' ||
      c.effectiveProxySource === 'resource_disabled' ||
      c.effectiveProxySource === 'resource_missing')
  ) {
    return `${label}：${c.proxyResourceName}`
  }
  return label
}

export function concurrencyLimitLabel(c: Pick<CredentialStatusItem, 'maxConcurrentRequests' | 'maxConcurrentRequestsOverride'>): string {
  if (typeof c.maxConcurrentRequestsOverride === 'number') {
    return c.maxConcurrentRequestsOverride > 0
      ? `账号覆盖：${c.maxConcurrentRequestsOverride}`
      : '账号覆盖：不限'
  }
  const effective = c.maxConcurrentRequests > 0 ? `${c.maxConcurrentRequests}` : '不限'
  return `继承全局：${effective}`
}

export function dispatchStatusLabel(
  c: Pick<
    CredentialStatusItem,
    'cooledDown' | 'cooldownRemainingSecs' | 'rateLimited' | 'rateLimitRemainingSecs' | 'maxConcurrentRequests' | 'inFlightRequests' | 'inProbation' | 'warmupRemaining'
  >,
  probationRemainingSecs: number
): string {
  if (c.cooledDown) return `冷却 ${c.cooldownRemainingSecs}s`
  if (c.rateLimited) return `限流 ${c.rateLimitRemainingSecs}s`
  if (c.maxConcurrentRequests > 0 && c.inFlightRequests >= c.maxConcurrentRequests)
    return `并发满 ${c.inFlightRequests}/${c.maxConcurrentRequests}`
  if (c.inProbation) return `观察期 ${probationRemainingSecs}s`
  if (c.warmupRemaining > 0) return `预热 ${c.warmupRemaining}`
  return '可调度'
}

export function formatResetAt(value?: number | null): string {
  if (!value) return '-'
  return new Date(value * 1000).toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

export function numberOrZero(v: number | null | undefined): number {
  return typeof v === 'number' && Number.isFinite(v) ? v : 0
}

export function accountInfoValue(
  c: Pick<CredentialStatusItem, 'accountInfo'>,
  balance?: BalanceResponse
): CredentialAccountInfo | undefined {
  return (balance as CredentialAccountInfo | undefined) || c.accountInfo
}

// ============================================================================
// Data merging
// ============================================================================

export function mapById<T extends { id: number }>(items: T[] | undefined): Map<number, T> {
  return new Map((items || []).map((item) => [item.id, item]))
}

export function mergeCredentialPlanes(
  base: CredentialListItem,
  runtime?: CredentialRuntimeItem,
  accountInfo?: CredentialAccountInfo,
  usage?: CredentialUsageSummaryItem
): CredentialStatusItem {
  return {
    ...base,
    failureCount: runtime?.failureCount ?? 0,
    isCurrent: runtime?.isCurrent ?? false,
    expiresAt: runtime?.expiresAt ?? null,
    accountInfo,
    successCount: runtime?.successCount ?? 0,
    lastUsedAt: runtime?.lastUsedAt ?? null,
    refreshFailureCount: runtime?.refreshFailureCount ?? 0,
    cooledDown: runtime?.cooledDown ?? false,
    cooldownRemainingSecs: runtime?.cooldownRemainingSecs ?? 0,
    cooldownReason: runtime?.cooldownReason,
    cooldowns: runtime?.cooldowns ?? [],
    rateLimited: runtime?.rateLimited ?? false,
    rateLimitRemainingSecs: runtime?.rateLimitRemainingSecs ?? 0,
    inFlightRequests: runtime?.inFlightRequests ?? 0,
    oldestInFlightAgeSecs: runtime?.oldestInFlightAgeSecs ?? 0,
    newestInFlightIdleSecs: runtime?.newestInFlightIdleSecs ?? 0,
    maxConcurrentRequests: runtime?.maxConcurrentRequests ?? base.maxConcurrentRequests,
    inFlightLeaseMaxSecs: runtime?.inFlightLeaseMaxSecs ?? 0,
    transientFailureStreak: runtime?.transientFailureStreak ?? 0,
    recentErrorRate: runtime?.recentErrorRate ?? 0,
    latencyEwmaMs: runtime?.latencyEwmaMs ?? null,
    lastErrorKind: runtime?.lastErrorKind,
    lastErrorReason: runtime?.lastErrorReason,
    lastErrorAtMs: runtime?.lastErrorAtMs ?? null,
    inProbation: runtime?.inProbation ?? false,
    probationRemainingSecs: runtime?.probationRemainingSecs ?? 0,
    schedulerSelectionCount: runtime?.schedulerSelectionCount ?? 0,
    recentSchedulerSelectionCount10s: runtime?.recentSchedulerSelectionCount10s ?? 0,
    recentSchedulerSelectionCount60s: runtime?.recentSchedulerSelectionCount60s ?? 0,
    recentSchedulerSelectionCount5m: runtime?.recentSchedulerSelectionCount5m ?? 0,
    schedulerSelectionPressure: runtime?.schedulerSelectionPressure ?? 0,
    schedulerScore: runtime?.schedulerScore ?? 0,
    estimatedCostUsd: usage?.estimatedCostUsd ?? 0,
    originalCostUsd: usage?.originalCostUsd ?? 0,
    kiroMeteringUsage: usage?.kiroMeteringUsage ?? 0,
    pricedRequests: usage?.pricedRequests ?? 0,
    unpricedRequests: usage?.unpricedRequests ?? 0,
  }
}
