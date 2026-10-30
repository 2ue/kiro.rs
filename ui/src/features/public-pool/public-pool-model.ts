// Display helpers for the public read-only pool page. Only type imports from outside, so they can
// be tested with `node --test`; the rules are the account page's, applied to the public fields.
import type { PublicPoolAccount, PublicPoolOverview, PublicPoolStatus } from '@/types/api'
import {
  credentialFlags,
  credentialStatusMeta,
  type CredentialFlag,
  type CredentialStatusMeta,
} from '../credentials/credential-utils.ts'

/** The account page's status tabs, in its order. */
export const PUBLIC_STATUS_TABS: ReadonlyArray<{
  value: PublicPoolStatus | '__all__'
  label: string
  dot: string
  count: (overview: PublicPoolOverview) => number
}> = [
  { value: '__all__', label: '全部', dot: 'bg-muted-foreground/60', count: (o) => o.total },
  { value: 'normal', label: '正常', dot: 'bg-success', count: (o) => o.normal },
  { value: 'cooling', label: '冷却', dot: 'bg-warning', count: (o) => o.cooling },
  { value: 'quota_exhausted', label: '额度用尽', dot: 'bg-destructive', count: (o) => o.quotaExhausted },
  { value: 'account_error', label: '账号异常', dot: 'bg-destructive', count: (o) => o.accountError },
  { value: 'disabled', label: '已禁用', dot: 'bg-muted-foreground/40', count: (o) => o.disabled },
]

/** The masked email; accounts without one (API Key) go by their id. */
export function publicAccountLabel(account: Pick<PublicPoolAccount, 'id' | 'label'>): string {
  return account.label?.trim() || `账号 #${account.id}`
}

/** The state a card leads with, by the account page's rules. */
export function publicStatusMeta(account: PublicPoolAccount): CredentialStatusMeta {
  return credentialStatusMeta({
    disabled: account.disabled,
    disabledReason: account.disabledReason,
    cooledDown: account.cooledDown,
    cooldownRemainingSecs: account.cooldownRemainingSecs,
    cooldownReason: undefined,
    cooldowns: account.cooldowns ?? [],
    rateLimited: account.rateLimited,
    rateLimitRemainingSecs: account.rateLimitRemainingSecs,
    inFlightRequests: account.inFlightRequests,
    maxConcurrentRequests: account.maxConcurrentRequests,
    inProbation: account.inProbation,
    probationRemainingSecs: account.probationRemainingSecs,
  })
}

/**
 * Markers that need a look (current, error streak, warmup). Settings markers (route, proxy,
 * models) are not part of the public data, so they never show.
 */
export function publicAttentionFlags(account: PublicPoolAccount): CredentialFlag[] {
  return credentialFlags({
    isCurrent: account.isCurrent,
    disabled: account.disabled,
    transientFailureStreak: account.transientFailureStreak,
    lastErrorReason: undefined,
    warmupRemaining: account.warmupRemaining,
    routeMode: 'allow_all',
    routeRules: [],
    hasProxy: false,
    effectiveProxySource: 'none',
    proxyResourceName: undefined,
    supportedModels: [],
    rateLimitAutoDisableEnabled: true,
    hasProfileArn: false,
  }).filter((flag) => flag.kind === 'attention')
}

