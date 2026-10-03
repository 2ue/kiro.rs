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

export type CredentialSubscriptionKey =
  | 'free'
  | 'students'
  | 'pro'
  | 'pro_plus'
  | 'pro_max'
  | 'power'
  | 'unknown'

export const CREDENTIAL_SUBSCRIPTION_OPTIONS: Array<{
  value: Exclude<CredentialSubscriptionKey, 'unknown'>
  label: string
}> = [
  { value: 'free', label: 'Kiro Free' },
  { value: 'students', label: 'Kiro Students' },
  { value: 'pro', label: 'Kiro Pro' },
  { value: 'pro_plus', label: 'Kiro Pro+' },
  { value: 'pro_max', label: 'Kiro Pro Max' },
  { value: 'power', label: 'Kiro Power' },
]

const CREDENTIAL_SUBSCRIPTION_LABELS: Record<CredentialSubscriptionKey, string> = {
  free: 'Kiro Free',
  students: 'Kiro Students',
  pro: 'Kiro Pro',
  pro_plus: 'Kiro Pro+',
  pro_max: 'Kiro Pro Max',
  power: 'Kiro Power',
  unknown: '未知订阅',
}

export function credentialSubscriptionKey(title?: string | null): CredentialSubscriptionKey {
  const normalized = (title || '').trim().toLowerCase()
  if (!normalized) return 'unknown'
  const compact = normalized.replace(/[^a-z0-9]/g, '')
  if (compact.includes('student')) return 'students'
  if (compact.includes('power')) return 'power'
  if (compact.includes('promax')) return 'pro_max'
  if (normalized.includes('pro+') || compact.includes('proplus')) return 'pro_plus'
  if (normalized.includes('trial') || normalized.includes('试用')) return 'free'
  if (normalized.includes('free') || normalized.includes('免费')) return 'free'
  if (compact.includes('pro')) return 'pro'
  return 'unknown'
}

export function credentialSubscriptionLabel(title?: string | null): string {
  const key = credentialSubscriptionKey(title)
  if (key !== 'unknown') return CREDENTIAL_SUBSCRIPTION_LABELS[key]
  return title?.trim() || CREDENTIAL_SUBSCRIPTION_LABELS.unknown
}

export function subscriptionBadgeMeta(
  cred: Pick<CredentialStatusItem, 'subscriptionTitle' | 'accountInfo'>,
  balance?: BalanceResponse
): { label: string; tone: BadgeTone; title?: string } {
  const raw = balance?.subscriptionTitle || cred.accountInfo?.subscriptionTitle || cred.subscriptionTitle || ''
  const key = credentialSubscriptionKey(raw)
  const label = credentialSubscriptionLabel(raw)
  if (key === 'power' || key === 'pro_max' || key === 'pro_plus' || key === 'pro') {
    return { label, tone: 'primary', title: raw || undefined }
  }
  if (key === 'students') return { label, tone: 'info', title: raw || undefined }
  if (key === 'free') return { label, tone: 'secondary', title: raw || undefined }
  return {
    label: label.length > 12 ? label.slice(0, 12) + '…' : label,
    tone: 'neutral',
    title: raw || undefined,
  }
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

const CREDIT_UNAVAILABLE_PATTERNS = [
  /\b403\b/i,
  /forbidden/i,
  /permission/i,
  /not[\s_-]*authori[sz]ed/i,
  /access[\s_-]*denied/i,
  /\bauth(?:entication|orization)?\b/i,
  /account[\s_-]*(?:suspend|lock)/i,
  /invalid[_\s-]*(?:token|refresh[_\s-]*token)/i,
]

/**
 * A stored balance is historical data. Only count it as spendable when the
 * account is still usable; historical usage and cost remain valid even for
 * accounts that later became unavailable.
 */
export function credentialCreditStatus(
  credential: Pick<CredentialStatusItem, 'disabled' | 'disabledReason' | 'lastErrorKind' | 'lastErrorReason'>
): { available: boolean; reason?: string } {
  if (credential.disabled) {
    return { available: false, reason: credential.disabledReason || '账号已禁用' }
  }
  const errorText = [credential.lastErrorKind, credential.lastErrorReason]
    .filter((value): value is string => Boolean(value?.trim()))
    .join(' ')
  if (errorText && CREDIT_UNAVAILABLE_PATTERNS.some((pattern) => pattern.test(errorText))) {
    return { available: false, reason: errorText }
  }
  return { available: true }
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
