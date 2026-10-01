import type { CredentialAccountInfo, CredentialCooldown } from './credential-quota'
import type { SetCredentialProxyRequest } from './proxies'
import type { UsageRecord } from './usage-records'

export interface CredentialsStatusResponse {
  total: number
  available: number
  currentId: number
  globalInFlightRequests: number
  queuedRequests: number
  globalMaxConcurrentRequests: number
  maxQueuedRequests: number
  credentials: CredentialStatusItem[]
}

export interface CredentialsPageResponse extends CredentialsStatusResponse {
  page: number
  limit: number
  totalPages: number
  filteredTotal: number
  filteredAvailable: number
}

export interface CredentialListResponse {
  page: number
  limit: number
  total: number
  available: number
  filteredTotal: number
  filteredAvailable: number
  totalPages: number
  items: CredentialListItem[]
}

export interface CredentialSummaryResponse {
  total: number
  available: number
  disabled: number
  currentId: number | null
  globalInFlightRequests: number
  queuedRequests: number
  globalMaxConcurrentRequests: number
  maxQueuedRequests: number
  updatedAt: string
  runtimeFresh: boolean
  schedulable: number
  coolingDown: number
  inUse: number
  failing: number
}

export interface SystemVersionResponse {
  version: string
}

export type CredentialSortBy =
  | 'default'
  | 'id'
  | 'created_at'
  | 'updated_at'
  | 'priority'
  | 'last_used_at'
  | 'success_count'
  | 'failure_count'
  | 'refresh_failure_count'
  | 'estimated_cost'
  | 'usage_percentage'
  | 'remaining_quota'
  | 'in_flight_requests'
  | 'scheduler_score'

export type CredentialSortOrder = 'asc' | 'desc'

export interface CredentialsPageQuery {
  page: number
  limit: number
  q?: string
  credentialId?: number
  account?: string
  region?: string
  model?: string
  endpoint?: string
  priority?: number
  rpm?: number
  concurrency?: number
  status?: string
  authMethod?: string
  subscription?: string
  proxyResourceId?: number
  sortBy?: CredentialSortBy
  sortOrder?: CredentialSortOrder
}

export interface CredentialStatusItem {
  id: number
  createdAt: string | null
  updatedAt: string | null
  priority: number
  disabled: boolean
  failureCount: number
  isCurrent: boolean
  expiresAt: string | null
  authMethod: string | null
  provider?: string
  region?: string
  authRegion?: string
  apiRegion?: string
  effectiveAuthRegion: string
  effectiveApiRegion: string
  hasProfileArn: boolean
  email?: string
  refreshTokenHash?: string
  apiKeyHash?: string
  maskedApiKey?: string
  subscriptionTitle?: string
  accountInfo?: CredentialAccountInfo
  successCount: number
  lastUsedAt: string | null
  hasProxy: boolean
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  proxyResourceId?: number
  proxyResourceName?: string
  effectiveProxyUrl?: string
  effectiveProxySource: 'credential' | 'resource' | 'resource_disabled' | 'resource_missing' | 'global' | 'direct' | 'none'
  refreshFailureCount: number
  disabledReason?: string
  endpoint: string
  cooledDown: boolean
  cooldownRemainingSecs: number
  cooldownReason?: string
  cooldowns?: CredentialCooldown[]
  rateLimited: boolean
  rateLimitRemainingSecs: number
  inFlightRequests: number
  oldestInFlightAgeSecs: number
  newestInFlightIdleSecs: number
  maxConcurrentRequests: number
  maxConcurrentRequestsOverride?: number
  inFlightLeaseMaxSecs: number
  warmupRemaining: number
  transientFailureStreak?: number
  recentErrorRate?: number
  latencyEwmaMs?: number | null
  lastErrorKind?: string
  lastErrorReason?: string
  lastErrorAtMs?: number | null
  supportedModels?: string[]
  tags?: string[]
  inProbation?: boolean
  probationRemainingSecs?: number
  schedulerSelectionCount?: number
  recentSchedulerSelectionCount10s?: number
  recentSchedulerSelectionCount60s?: number
  recentSchedulerSelectionCount5m?: number
  schedulerSelectionPressure?: number
  schedulerScore?: number
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage: number
  pricedRequests: number
  unpricedRequests: number
  rpm: number
  rpmOverride?: number
  rateLimitAutoDisableEnabled: boolean
}

export type CredentialListItem = Pick<
  CredentialStatusItem,
  | 'id'
  | 'createdAt'
  | 'updatedAt'
  | 'priority'
  | 'disabled'
  | 'authMethod'
  | 'provider'
  | 'region'
  | 'authRegion'
  | 'apiRegion'
  | 'effectiveAuthRegion'
  | 'effectiveApiRegion'
  | 'hasProfileArn'
  | 'email'
  | 'refreshTokenHash'
  | 'apiKeyHash'
  | 'maskedApiKey'
  | 'subscriptionTitle'
  | 'hasProxy'
  | 'proxyUrl'
  | 'proxyUsername'
  | 'proxyPassword'
  | 'proxyResourceId'
  | 'proxyResourceName'
  | 'effectiveProxyUrl'
  | 'effectiveProxySource'
  | 'disabledReason'
  | 'endpoint'
  | 'maxConcurrentRequests'
  | 'maxConcurrentRequestsOverride'
  | 'rpm'
  | 'rpmOverride'
  | 'rateLimitAutoDisableEnabled'
  | 'warmupRemaining'
  | 'supportedModels'
  | 'tags'
>

export type CredentialRuntimeItem = Pick<
  CredentialStatusItem,
  | 'id'
  | 'failureCount'
  | 'isCurrent'
  | 'expiresAt'
  | 'successCount'
  | 'lastUsedAt'
  | 'refreshFailureCount'
  | 'cooledDown'
  | 'cooldownRemainingSecs'
  | 'cooldownReason'
  | 'cooldowns'
  | 'rateLimited'
  | 'rateLimitRemainingSecs'
  | 'inFlightRequests'
  | 'oldestInFlightAgeSecs'
  | 'newestInFlightIdleSecs'
  | 'maxConcurrentRequests'
  | 'rpm'
  | 'inFlightLeaseMaxSecs'
  | 'transientFailureStreak'
  | 'recentErrorRate'
  | 'latencyEwmaMs'
  | 'lastErrorKind'
  | 'lastErrorReason'
  | 'lastErrorAtMs'
  | 'supportedModels'
  | 'inProbation'
  | 'probationRemainingSecs'
  | 'schedulerSelectionCount'
  | 'recentSchedulerSelectionCount10s'
  | 'recentSchedulerSelectionCount60s'
  | 'recentSchedulerSelectionCount5m'
  | 'schedulerSelectionPressure'
  | 'schedulerScore'
>

export interface CredentialRuntimeResponse {
  items: CredentialRuntimeItem[]
  updatedAt: string
  fresh: boolean
}

export interface CredentialDiagnosticsResponse {
  credentialId: number
  runtime?: CredentialRuntimeItem
  page: number
  limit: number
  hasNext: boolean
  records: UsageRecord[]
  generatedAt: string
}

export interface SuccessResponse {
  success: boolean
  message: string
}

export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

export interface SetWarmupRequest {
  warmupRemaining: number
}

export interface SetCredentialConcurrencyRequest {
  maxConcurrentRequests?: number | null
}

export interface SetCredentialRpmRequest {
  rpm?: number | null
}

export interface SetCredentialRateLimitAutoDisableRequest {
  enabled: boolean
}

export interface SetCredentialRegionsRequest {
  region?: string | null
  authRegion?: string | null
  apiRegion?: string | null
}

export interface BatchUpdateCredentialsRequest {
  ids: number[]
  priority?: SetPriorityRequest
  regions?: SetCredentialRegionsRequest
  concurrency?: SetCredentialConcurrencyRequest
  rpm?: SetCredentialRpmRequest
  rateLimitAutoDisable?: SetCredentialRateLimitAutoDisableRequest
  proxy?: SetCredentialProxyRequest
}

export interface BatchUpdateCredentialItem {
  id: number
  ok: boolean
  error?: string
}

export interface BatchUpdateCredentialsResponse {
  total: number
  success: number
  failed: number
  items: BatchUpdateCredentialItem[]
}
