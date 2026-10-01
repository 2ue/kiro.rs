import type { CredentialStatusItem } from './credentials'

export type CredentialAccountInfoItem = CredentialAccountInfo & {
  id: number
}

export interface CredentialAccountInfoListResponse {
  items: CredentialAccountInfoItem[]
  updatedAt: string
  fresh: boolean
}

export type CredentialUsageSummaryItem = Pick<
  CredentialStatusItem,
  'id' | 'estimatedCostUsd' | 'originalCostUsd' | 'kiroMeteringUsage' | 'pricedRequests' | 'unpricedRequests'
>

export interface CredentialUsageSummaryResponse {
  items: CredentialUsageSummaryItem[]
  updatedAt: string
  fresh: boolean
}

export interface UsageDashboardAccountItem {
  id: number
  email?: string | null
  label: string
  authMethod?: string | null
  provider?: string | null
  endpoint: string
  subscriptionTitle?: string | null
  disabled: boolean
  isCurrent: boolean
  inFlightRequests: number
  rateLimited: boolean
  cooledDown: boolean
  rpm: number
  maxConcurrentRequests: number
  creditLimit?: number | null
  creditRemaining?: number | null
  creditUsed?: number | null
  accountInfoCheckedAt?: string | null
  windowRequests: number
  windowErrorRequests: number
  windowTotalInputTokens: number
  windowTotalOutputTokens: number
  windowEstimatedCostUsd: number
  windowOriginalCostUsd: number
  windowKiroMeteringUsage: number
  windowPricedRequests: number
  windowUnpricedRequests: number
  lifetimeRequests: number
  lifetimeErrorRequests: number
  lifetimeTotalInputTokens: number
  lifetimeTotalOutputTokens: number
  lifetimeEstimatedCostUsd: number
  lifetimeOriginalCostUsd: number
  lifetimeKiroMeteringUsage: number
  lifetimePricedRequests: number
  lifetimeUnpricedRequests: number
}

export interface UsageDashboardAccountsResponse {
  generatedAt: string
  timezone: string
  windowKey: string
  page: number
  pageSize: number
  total: number
  filteredTotal: number
  totalPages: number
  configuredLocalAccounts: number
  windowActiveLocalAccounts: number
  windowIdleLocalAccounts: number
  complete: boolean
  reason?: string
  items: UsageDashboardAccountItem[]
}

export interface BulkCredentialActionResponse {
  totalMatched: number
  totalAttempted: number
  success: number
  failed: number
  skipped: number
  errors: Array<{ id: number; message: string }>
}

export interface CredentialCooldown {
  model?: string
  global: boolean
  remainingSecs: number
  reason?: string
}

export interface CredentialAccountInfo {
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  creditLimit: number
  creditRemaining: number
  creditBase: number
  creditBonus: number
  overageStatus?: string | null
  overageCapability?: string | null
  overageCap: number
  overageRate: number
  currentOverages: number
  nextResetAt: number | null
  checkedAt: string
}

export interface BalanceResponse {
  id: number
  checkedAt: string
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  creditLimit: number
  creditRemaining: number
  creditBase: number
  creditBonus: number
  overageStatus?: string | null
  overageCapability?: string | null
  overageCap: number
  overageRate: number
  currentOverages: number
  nextResetAt: number | null
}

export type CredentialInfoResponse = BalanceResponse

export interface CredentialCreditSummaryResponse {
  totalCredentials: number
  enabledCredentials: number
  disabledCredentials: number
  totalCreditLimit: number
  totalCreditRemaining: number
  totalCurrentUsage: number
  enabledCreditLimit: number
  enabledCreditRemaining: number
  disabledCreditLimit: number
  disabledCreditRemaining: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
  enabledEstimatedCostUsd: number
  enabledOriginalCostUsd: number
  disabledEstimatedCostUsd: number
  disabledOriginalCostUsd: number
  lastCheckedAt: string | null
}

export interface RefreshCredentialInfoRequest {
  ids: number[]
  force?: boolean
}

export interface CredentialInfoRefreshItem {
  id: number
  email?: string | null
  disabled: boolean
  ok: boolean
  info?: CredentialInfoResponse | null
  error?: string | null
}

export interface CredentialInfoRefreshResponse {
  total: number
  success: number
  failed: number
  items: CredentialInfoRefreshItem[]
}
