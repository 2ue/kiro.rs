import type {
  BalanceResponse,
  CredentialAccountInfo,
  CredentialListItem,
  CredentialRuntimeItem,
  CredentialStatusItem,
  CredentialUsageSummaryItem,
  ExternalPoolRouteMode,
} from '@/types/api'

// ============================================================================
// Label helpers
// ============================================================================

export function credentialLabel(
  c: Pick<CredentialStatusItem, 'id' | 'email' | 'maskedApiKey'>,
  showEmailSuffix = true,
) {
  const email = c.email?.trim()
  if (email) {
    if (showEmailSuffix) return email
    const at = email.indexOf('@')
    return at > 0 ? `${email.slice(0, at)}***` : `${email}***`
  }
  return c.maskedApiKey || `账号 #${c.id}`
}

export function authLabel(authMethod: string | null | undefined) {
  if (authMethod === 'api_key') return 'API Key'
  if (authMethod === 'idc') return 'IdC'
  if (authMethod === 'external_idp') return 'External IdP'
  if (authMethod === 'social') return 'Social'
  return authMethod || 'Unknown'
}

export type BadgeTone = 'neutral' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' | 'info'

/** Short label for a credential's route policy; undefined when it accepts every route. */
export function credentialRouteSummary(mode?: ExternalPoolRouteMode, rules: string[] = []): string | undefined {
  if (!mode || mode === 'allow_all') return undefined
  const list = rules.length <= 2 ? rules.join(', ') : `${rules[0]}, ${rules[1]} 等 ${rules.length} 条`
  if (mode === 'allow_list') return rules.length ? `仅 ${list}` : '不承接任何入口'
  return rules.length ? `排除 ${list}` : undefined
}

/** The one state a card leads with; the most blocking condition wins. */
export function credentialStatusMeta(
  c: Pick<
    CredentialStatusItem,
    | 'disabled'
    | 'disabledReason'
    | 'cooledDown'
    | 'cooldownRemainingSecs'
    | 'cooldownReason'
    | 'rateLimited'
    | 'rateLimitRemainingSecs'
    | 'inFlightRequests'
    | 'maxConcurrentRequests'
    | 'inProbation'
    | 'probationRemainingSecs'
  >,
): { label: string; tone: BadgeTone; title?: string } {
  if (c.disabled) {
    const manual = !c.disabledReason || c.disabledReason === 'Manual'
    return { label: disabledReasonLabel(c.disabledReason), tone: manual ? 'neutral' : 'error', title: c.disabledReason || undefined }
  }
  if (c.cooledDown) {
    return { label: c.cooldownRemainingSecs > 0 ? `冷却 ${c.cooldownRemainingSecs}s` : '冷却', tone: 'warning', title: c.cooldownReason || undefined }
  }
  if (c.rateLimited) {
    return { label: c.rateLimitRemainingSecs > 0 ? `限流 ${c.rateLimitRemainingSecs}s` : '限流', tone: 'info' }
  }
  if (c.maxConcurrentRequests > 0 && c.inFlightRequests >= c.maxConcurrentRequests) {
    return { label: '并发已满', tone: 'warning', title: `${c.inFlightRequests}/${c.maxConcurrentRequests}` }
  }
  if (c.inProbation) {
    const secs = numberOrZero(c.probationRemainingSecs)
    return { label: '观察期', tone: 'info', title: secs > 0 ? `剩余 ${secs}s` : undefined }
  }
  return { label: '正常', tone: 'success' }
}

export interface CredentialFlag {
  key: 'current' | 'errors' | 'warmup' | 'route' | 'proxy' | 'models' | 'rate-limit' | 'profile-arn'
  /** `attention` needs a look and is spelled out; `config` is a standing setting shown as an icon. */
  kind: 'attention' | 'config'
  label: string
  tone: BadgeTone
  title?: string
  /** A number worth showing next to an icon, e.g. how many models the account is limited to. */
  count?: number
}

/** Secondary markers in the order a card shows them. */
export function credentialFlags(
  c: Pick<
    CredentialStatusItem,
    | 'isCurrent'
    | 'disabled'
    | 'transientFailureStreak'
    | 'lastErrorReason'
    | 'warmupRemaining'
    | 'routeMode'
    | 'routeRules'
    | 'hasProxy'
    | 'effectiveProxySource'
    | 'proxyResourceName'
    | 'supportedModels'
    | 'rateLimitAutoDisableEnabled'
    | 'hasProfileArn'
  >,
): CredentialFlag[] {
  const flags: CredentialFlag[] = []
  if (c.isCurrent) flags.push({ key: 'current', kind: 'attention', label: '当前', tone: 'primary', title: '调度器当前选中的账号' })
  const streak = numberOrZero(c.transientFailureStreak)
  if (!c.disabled && streak > 0) {
    flags.push({ key: 'errors', kind: 'attention', label: `连续错误 ${streak}`, tone: 'error', title: c.lastErrorReason || undefined })
  }
  if (c.warmupRemaining > 0) {
    flags.push({ key: 'warmup', kind: 'attention', label: `预热 ${c.warmupRemaining}`, tone: 'warning', title: `还需成功 ${c.warmupRemaining} 次才完全参与调度` })
  }
  const route = credentialRouteSummary(c.routeMode, c.routeRules)
  if (route) flags.push({ key: 'route', kind: 'config', label: route, tone: 'secondary', title: `路由限制：${route}` })
  if (c.hasProxy) flags.push({ key: 'proxy', kind: 'config', label: '代理', tone: 'info', title: `代理：${proxySummary(c)}` })
  const models = c.supportedModels ?? []
  if (models.length > 0) flags.push({ key: 'models', kind: 'config', label: `模型 ${models.length}`, tone: 'secondary', count: models.length, title: `只调度 ${models.length} 个模型：${models.join('、')}` })
  if (c.rateLimitAutoDisableEnabled === false) {
    flags.push({ key: 'rate-limit', kind: 'config', label: '429仅冷却', tone: 'warning', title: '遇到 429 只冷却，不自动禁用' })
  }
  if (c.hasProfileArn) flags.push({ key: 'profile-arn', kind: 'config', label: 'Profile ARN', tone: 'neutral', title: '已配置 Profile ARN' })
  return flags
}

/** Colour for a used/limit ratio; a limit of 0 means unlimited and never warns. */
export function loadTone(used: number, limit: number): 'warning' | 'error' | undefined {
  if (!(limit > 0)) return undefined
  const ratio = used / limit
  if (ratio >= 1) return 'error'
  if (ratio >= 0.7) return 'warning'
  return undefined
}

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
  { value: 'free', label: 'Free' },
  { value: 'students', label: 'Students' },
  { value: 'pro', label: 'Pro' },
  { value: 'pro_plus', label: 'Pro+' },
  { value: 'pro_max', label: 'Pro Max' },
  { value: 'power', label: 'Power' },
]

const CREDENTIAL_SUBSCRIPTION_LABELS: Record<CredentialSubscriptionKey, string> = {
  free: 'Free',
  students: 'Students',
  pro: 'Pro',
  pro_plus: 'Pro+',
  pro_max: 'Pro Max',
  power: 'Power',
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
): { key: CredentialSubscriptionKey; label: string; tone: BadgeTone; title?: string } {
  const raw = balance?.subscriptionTitle || cred.accountInfo?.subscriptionTitle || cred.subscriptionTitle || ''
  const key = credentialSubscriptionKey(raw)
  const label = credentialSubscriptionLabel(raw)
  if (key === 'power' || key === 'pro_max' || key === 'pro_plus' || key === 'pro') {
    return { key, label, tone: 'primary', title: raw || undefined }
  }
  if (key === 'students') return { key, label, tone: 'info', title: raw || undefined }
  if (key === 'free') return { key, label, tone: 'secondary', title: raw || undefined }
  return {
    key,
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

/** Chinese labels for the scheduler's disable reasons; unknown values are shown as they are. */
const DISABLED_REASON_LABEL: Record<string, string> = {
  Manual: '手动禁用',
  TooManyFailures: '连续失败禁用',
  TooManyRefreshFailures: '刷新失败禁用',
  QuotaExceeded: '额度用尽',
  InvalidRefreshToken: 'Token 失效',
  InvalidConfig: '配置无效',
  TemporarilySuspended: '账号暂停',
  AccountSuspended: '账号封禁',
  AccountLocked: '账号锁定',
}

export function disabledReasonLabel(reason?: string | null): string {
  if (!reason) return '禁用'
  return DISABLED_REASON_LABEL[reason] ?? reason
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
    return { available: false, reason: credential.disabledReason ? disabledReasonLabel(credential.disabledReason) : '账号已禁用' }
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

// ============================================================================
// Quota bar
// ============================================================================

export type QuotaTone = 'success' | 'warning' | 'error'

export interface QuotaBarModel {
  /** Base usage as a share of the plan limit, 0-100. */
  basePercent: number
  baseTone: QuotaTone
  /** Usage went past the plan limit. */
  inOverage: boolean
  /** Credits used beyond the plan limit; 0 while within the limit. */
  overage: number
  /** Total usage (plan + overage) as a share of the plan limit; above 100 in overage. */
  usagePercent: number
  /** Widths in % of the whole track. Within the limit the base fills `basePercent`. */
  baseWidth: number
  overageWidth: number
}

/** Track share the full base segment keeps however large the overage grows. */
const MIN_BASE_WIDTH_WHEN_OVERAGE = 40
/** Track share the overage segment keeps however small the overage is, so it stays visible. */
const MIN_OVERAGE_WIDTH = 4

/**
 * Splits account usage into the plan part and the overage part, all in credits. Within the limit
 * this is the usual single bar. Past the limit the base segment is full and the overage follows
 * it on the same track, sized by `limit / (limit + overage)` so it grows with the actual
 * overage; both segments keep a minimum width so neither disappears.
 *
 * Overage is the larger of `currentUsage - usageLimit` and `currentOverages`: some responses
 * cap usage at the limit and report the excess only in `currentOverages`.
 */
export function quotaBarModel(
  info: Pick<CredentialAccountInfo, 'currentUsage' | 'usageLimit' | 'currentOverages'> | undefined,
): QuotaBarModel {
  const used = finiteNonNegative(info?.currentUsage)
  const limit = finiteNonNegative(info?.usageLimit)
  const overage = limit > 0 ? Math.max(used - limit, finiteNonNegative(info?.currentOverages), 0) : 0
  const basePercent = limit > 0 ? Math.min(100, (Math.min(used, limit) / limit) * 100) : 0
  const baseTone: QuotaTone = basePercent >= 95 ? 'error' : basePercent >= 80 ? 'warning' : 'success'
  if (overage <= 0) {
    return { basePercent, baseTone, inOverage: false, overage: 0, usagePercent: basePercent, baseWidth: basePercent, overageWidth: 0 }
  }
  const proportional = (limit / (limit + overage)) * 100
  const baseWidth = Math.min(100 - MIN_OVERAGE_WIDTH, Math.max(MIN_BASE_WIDTH_WHEN_OVERAGE, proportional))
  return {
    basePercent: 100,
    baseTone: 'error',
    inOverage: true,
    overage,
    usagePercent: ((limit + overage) / limit) * 100,
    baseWidth,
    overageWidth: 100 - baseWidth,
  }
}

function finiteNonNegative(value: number | null | undefined): number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : 0
}

/**
 * Quota as one reading: `32.8%  16.4/50` within the plan, `100%（+34.5%）  1,000(+345)/1,000` once
 * past it, so the overage sits next to the number it adds to instead of on a line of its own.
 * Brackets only appear when there is overage. Overage has no known cap, so none is shown.
 */
export function quotaReading(
  info: Pick<CredentialAccountInfo, 'currentUsage' | 'usageLimit'> | undefined,
  bar: QuotaBarModel,
): { percent: string; amount: string } | undefined {
  if (!info || !(info.usageLimit > 0)) return undefined
  const pct = (value: number) => `${(Math.round(value * 10) / 10).toString()}%`
  const limit = formatQuota1(info.usageLimit)
  if (!bar.inOverage) {
    return { percent: pct(bar.basePercent), amount: `${formatQuota1(info.currentUsage)}/${limit}` }
  }
  return {
    percent: `100%（+${pct((bar.overage / info.usageLimit) * 100)}）`,
    amount: `${limit}(+${formatQuota1(bar.overage)})/${limit}`,
  }
}

/** Credits with grouping and at most 6 decimals, matching `formatQuotaFixed` without importing the UI formatter. */
function formatQuota1(value: number): string {
  return new Intl.NumberFormat('zh-CN', { minimumFractionDigits: 0, maximumFractionDigits: 6, useGrouping: true }).format(value)
}

/** Below this many dispatches the ratio is too noisy to colour. */
const SUCCESS_RATE_MIN_SAMPLE = 20

/**
 * Successes against dispatches. Dispatches also count requests still in flight, cancelled by the
 * client or retried on another account, so the gap is "did not succeed here", not "failed". Older
 * stats can have fewer dispatches than successes; those read as fully successful.
 */
export function successRate(successCount: number, selectionCount: number): {
  dispatched: number
  percent?: number
  tone?: 'success' | 'warning' | 'error'
} {
  const success = Math.max(0, successCount || 0)
  const dispatched = Math.max(success, selectionCount || 0)
  if (dispatched === 0) return { dispatched }
  const percent = (success / dispatched) * 100
  if (dispatched < SUCCESS_RATE_MIN_SAMPLE) return { dispatched, percent }
  return { dispatched, percent, tone: percent >= 95 ? 'success' : percent >= 80 ? 'warning' : 'error' }
}
