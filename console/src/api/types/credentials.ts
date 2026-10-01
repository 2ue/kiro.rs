import type { SetCredentialProxyRequest } from './proxies'
import type { UsageRecord } from './usage'

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

export interface ValidateExistingCredentialsRequest {
  scope?: 'all' | 'enabled' | 'disabled' | 'selected'
  ids?: number[]
  force?: boolean
}

export interface ValidateExternalCredentialsRequest {
  credentials: AddCredentialRequest[]
  querySubscription?: boolean
  queryUsage?: boolean
  checkLiveness?: boolean
  livenessModel?: string
  livenessPrompt?: string
}

export interface CredentialValidationInfo {
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  usagePercentage: number
  checkedAt: string
}

export interface CredentialValidationItem {
  id?: number | null
  index?: number | null
  email?: string | null
  disabled?: boolean | null
  ok: boolean
  previous?: CredentialValidationInfo | null
  current?: CredentialValidationInfo | null
  changeKind: string
  subscriptionKey: string
  subscriptionTitle: string
  error?: string | null
  subscriptionChecked?: boolean
  usageChecked?: boolean
  livenessChecked?: boolean
  subscriptionOk?: boolean | null
  usageOk?: boolean | null
  livenessOk?: boolean | null
  usageError?: string | null
  livenessError?: string | null
  livenessModel?: string | null
  livenessResponse?: string | null
  matchedExistingCredentialId?: number | null
  existingDisabled?: boolean | null
}

export interface CredentialValidationGroup {
  key: string
  title: string
  count: number
  items: CredentialValidationItem[]
}

export interface CredentialValidationResponse {
  total: number
  success: number
  failed: number
  downgraded: number
  upgraded: number
  unchanged: number
  groups: CredentialValidationGroup[]
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

export interface AddCredentialRequest {
  accessToken?: string
  expiresAt?: string
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'external_idp' | 'api_key'
  provider?: string
  clientId?: string
  clientSecret?: string
  tokenEndpoint?: string
  issuerUrl?: string
  scopes?: string
  email?: string
  profileArn?: string
  priority?: number
  maxConcurrentRequests?: number | null
  rpm?: number | null
  rateLimitAutoDisableEnabled?: boolean | null
  disabled?: boolean | null
  enableOverageAfterImport?: boolean | null
  region?: string
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  proxyResourceId?: number | null
  kiroApiKey?: string
  endpoint?: string
  supportedModels?: string[]
  tags?: string[]
  autoDiscoverSupportedModels?: boolean | null
}

export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
  warning?: string
}

export interface TestCredentialRequest {
  model: string
  prompt?: string
}

export interface TestCredentialResponse {
  success: boolean
  credentialId: number
  model: string
  modelId: string
  prompt: string
  response: string
  durationMs: number
}

export interface BatchCredentialImportDefaults {
  disabled?: boolean
  priority?: number
  maxConcurrentRequests?: number | null
  rpm?: number | null
  rateLimitAutoDisableEnabled?: boolean
  provider?: string
  authRegion?: string
  apiRegion?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  proxyResourceId?: number | null
  /** 按账号原始顺序循环绑定的代理资源 ID；非空时优先于 proxyResourceId */
  proxyResourceIds?: number[]
  endpoint?: string
  warmupRemaining?: number
  enableOverageAfterImport?: boolean
  supportedModels?: string[]
  tags?: string[]
}

export interface BatchCredentialImportRequest {
  defaults?: BatchCredentialImportDefaults
  duplicateMode?: 'skip' | 'error'
  continueOnError?: boolean
  autoDiscoverSupportedModels?: boolean
  credentials: AddCredentialRequest[]
}

export interface BatchCredentialImportItem {
  index: number
  ok: boolean
  skipped: boolean
  credentialId?: number
  email?: string
  error?: string
  warning?: string
}

export interface BatchCredentialImportResponse {
  total: number
  success: number
  skipped: number
  failed: number
  items: BatchCredentialImportItem[]
}

export type DisabledReasonCode =
  | 'Manual'
  | 'TooManyFailures'
  | 'TooManyRefreshFailures'
  | 'QuotaExceeded'
  | 'InvalidRefreshToken'
  | 'InvalidConfig'
  | 'TemporarilySuspended'
  | 'AccountSuspended'
  | 'AccountLocked'
