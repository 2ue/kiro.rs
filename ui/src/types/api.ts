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

export type UsageCleanupMode = 'soft_delete' | 'hard_delete'
export type UsageCleanupJobStatus = 'idle' | 'running' | 'completed' | 'cancelled' | 'failed'

export interface UsageCleanupRequest {
  mode?: UsageCleanupMode
  olderThanDays?: number
  cutoffBefore?: string
  batchSize?: number
  maxBatches?: number
  pauseMsBetweenBatches?: number
}

export interface UsageCleanupPreviewResponse {
  mode: UsageCleanupMode
  cutoffAt: string
  matchedRows: number
  oldestCreatedAt?: string
  newestCreatedAt?: string
}

export interface UsageCleanupStatusResponse {
  jobId?: string
  status: UsageCleanupJobStatus
  mode?: UsageCleanupMode
  cutoffAt?: string
  batchSize: number
  maxBatches: number
  pauseMsBetweenBatches: number
  matchedRows?: number
  remainingRows?: number
  processedRows: number
  lastBatchRows: number
  batches: number
  cancelRequested: boolean
  stopReason?: string
  startedAt?: string
  updatedAt?: string
  finishedAt?: string
  lastError?: string
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

export interface ProxyResource {
  id: number
  name: string
  proxyUrl: string
  proxyUsername?: string | null
  proxyPassword?: string | null
  hasPassword: boolean
  enabled: boolean
  notes?: string | null
  createdAt: string
  updatedAt: string
  credentialCount: number
}

export interface ProxyResourcesResponse {
  resources: ProxyResource[]
}

export interface ProxyResourceTestRequest {
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  testUrl?: string
}

export interface ProxyResourceTestResponse {
  success: boolean
  message: string
  proxyUrl: string
  testUrl: string
  status?: number | null
  durationMs: number
  responsePreview?: string
}

export interface CreateProxyResourceRequest {
  name: string
  proxyUrl: string
  proxyUsername?: string
  proxyPassword?: string
  enabled?: boolean
  notes?: string
}

export interface UpdateProxyResourceRequest {
  name?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  clearUsername?: boolean
  clearPassword?: boolean
  enabled?: boolean
  notes?: string
  clearNotes?: boolean
}

export interface SetCredentialProxyRequest {
  proxyResourceId?: number | null
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
}

export type UsageRecordStatus =
  | 'success'
  | 'error'
  | 'stream_error'
  | 'upstream_timeout'
  | 'client_dropped'

export type UsageSource =
  | 'upstream_metadata'
  | 'local_prompt_cache'
  | 'context_estimate'
  | 'request_estimate'
  | 'none'

export type UsageRouteKindFilter = 'local_credential' | 'external_pool'

export interface UsageLatencyTrace {
  capacityWeightUnits?: number
  estimatedInputTokens?: number
  payloadGuardMs?: number
  upstreamHeaderMs?: number
  firstUpstreamChunkMs?: number
  firstOutputDeltaMs?: number
  firstThinkingDeltaMs?: number
  firstVisibleTextDeltaMs?: number
  streamGapToFirstOutputMs?: number
  chunksBeforeFirstOutput?: number
  eventsBeforeFirstOutput?: number
  upstreamBytesBeforeFirstOutput?: number
  upstreamFramesBeforeFirstOutput?: number
  upstreamEventsBeforeFirstOutput?: number
  upstreamFramesWithoutDownstreamEventsBeforeFirstOutput?: number
  upstreamPendingChunksBeforeFirstOutput?: number
  upstreamFrameDecodeErrorsBeforeFirstOutput?: number
  upstreamEventParseErrorsBeforeFirstOutput?: number
  upstreamEventTypesBeforeFirstOutput?: Record<string, number>
  clientDroppedMs?: number
  terminalReason?: 'completed' | 'upstream_status_error' | 'upstream_json_exception' | 'upstream_idle_timeout' | 'malformed_sse' | 'client_dropped' | 'internal_error'
  upstreamMessageStatus?: string
  sawUpstreamCompleted?: boolean
  stopReasonSource?: string
  suspectedIntentPreambleEndTurn?: boolean
}

export interface KiroCredentialAttempt {
  attempt: number
  credentialId: number
  credentialLabel?: string
  status?: number
  statusText?: string
  action: string
  model?: string
  errorType?: string
  errorMessage?: string
  durationMs: number
}

export interface UsageRecord {
  id: string
  createdAt: string
  endpoint: string
  stream: boolean
  model: string
  requestedMaxTokens?: number
  upstreamModel?: string
  externalOutboundModel?: string
  modelResolutionSource?: string
  modelResolutionNote?: string
  conversationId?: string
  credentialId?: number
  credentialLabel?: string
  status: UsageRecordStatus
  usageSource: UsageSource
  totalInputTokens: number
  compatInputTokens: number
  billableInputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  cacheCreation5mInputTokens: number
  cacheCreation1hInputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage: number
  pricingAvailable: boolean
  pricingModel?: string
  durationMs: number
  firstTokenLatencyMs?: number
  responseLatencyMs?: number
  latencyTrace?: UsageLatencyTrace
  simulated: boolean
  stickyBound: boolean
  fallbackFromSticky: boolean
  credentialAttempts?: KiroCredentialAttempt[]
  routeKind?: 'local_credential' | 'external_pool'
  routeSubtype?: 'local_success' | 'local_error_no_fallback' | 'local_rescue_after_external' | 'external_fallback_preflight' | 'external_fallback_after_local_attempts' | 'external_direct_policy' | 'external_error'
  fallbackReason?: string
  directPolicyReason?: string
  localAttempted?: boolean
  localPreflight?: unknown
  externalPoolId?: number
  externalPoolName?: string
  externalAttempts?: ExternalPoolAttempt[]
  usageProjectionApplied?: boolean
  externalPoolBilling?: ExternalPoolBilling
  errorType?: string
  errorMessage?: string
  errorDetail?: string
  errorStatusCode?: number
  errorSource?: string
  errorId?: string
  errorMetadata?: unknown
  publicErrorStatusCode?: number
  publicErrorType?: string
  publicErrorMessage?: string
  payloadBreakdown?: unknown
  payloadGuardReport?: unknown
}

export interface UsageRecorderStats {
  inMemoryLimit: number
  inMemoryRecords: number
  redisEnabled: boolean
  redisQueueEnabled: boolean
  redisQueueCapacity: number
  redisQueueAvailable: number
  droppedRedisRecords: number
  postgresEnabled: boolean
  writerQueueEnabled: boolean
  writerQueueCapacity: number
  writerQueueAvailable: number
  droppedPersistRecords: number
}

export interface ExternalPoolUsageSnapshot {
  totalInputTokens: number
  inputTokens: number
  billableInputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  cacheCreation5mInputTokens: number
  cacheCreation1hInputTokens: number
}

export interface ExternalPoolBilling {
  requestInputTokens?: number
  rawUsage: ExternalPoolUsageSnapshot
  shapedUsage?: ExternalPoolUsageSnapshot
  reportedUsage: ExternalPoolUsageSnapshot
  usageProjectionApplied: boolean
  rawCostUsd: number
  shapedCostUsd?: number
  upliftedCostUsd?: number
  profitUsd?: number
  reportedCostUsd: number
  billableCostUsd: number
  costFloorDeltaUsd: number
  costFloorApplied: boolean
  pricingAvailable: boolean
  pricingModel?: string
  usageProjectionMode: string
  streamResponseMode?: ExternalPoolStreamResponseMode
}

export interface ExternalPoolAttempt {
  attempt: number
  poolId: number
  poolName: string
  outboundModel?: string
  status?: number
  action: string
  durationMs: number
  errorType?: string
  errorMessage?: string
}

export interface UsageRecordsResult {
  total: number
  records: UsageRecord[]
}

export interface UsageRecordsPageResult {
  page: number
  limit: number
  hasNext: boolean
  records: UsageRecord[]
}

export interface UsageAggregate {
  key: string
  label?: string
  requests: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
}

export interface UsageRealtimeStats {
  windowSeconds: number
  requests: number
  rpm: number
  inputTpm: number
  outputTpm: number
  totalTpm: number
  billableTpm: number
}

export interface UsageSummary {
  totalRequests: number
  successRequests: number
  errorRequests: number
  highCacheRequests: number
  totalInputTokens: number
  totalOutputTokens: number
  totalCacheReadInputTokens: number
  totalCacheCreationInputTokens: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
  pricedRequests: number
  unpricedRequests: number
  localPromptCacheRequests: number
  localPromptCacheInputTokens: number
  localPromptCacheReadInputTokens: number
  localPromptCacheCreationInputTokens: number
  simulatedRequests: number
  upstreamMetadataRequests: number
  externalPoolBilling?: UsageExternalPoolBillingSummary
  realtime: UsageRealtimeStats
  topCredentials: UsageAggregate[]
  topConversations: UsageAggregate[]
}

export interface UsageExternalPoolBillingSummary {
  requests: number
  pricedRequests: number
  unpricedRequests: number
  costFloorAppliedRequests: number
  rawCostUsd: number
  shapedCostUsd?: number
  upliftedCostUsd?: number
  profitUsd?: number
  reportedCostUsd: number
  billableCostUsd: number
  costFloorDeltaUsd: number
}

export interface UsageExternalPoolBillingByPool extends UsageExternalPoolBillingSummary {
  poolId: number
  poolName: string
}

export interface UsageDashboardResponse {
  generatedAt: string
  timezone: string
  windows: UsageDashboardWindow[]
  series: UsageDashboardSeries
  top: UsageDashboardTop
}

export interface UsageDashboardWindowsResponse {
  generatedAt: string
  timezone: string
  windows: UsageDashboardWindow[]
}

export interface UsageDashboardSeriesResponse {
  generatedAt: string
  timezone: string
  series: UsageDashboardSeries
}

export interface UsageDashboardTopResponse {
  generatedAt: string
  top: UsageDashboardTop
}

export interface UsageDashboardBreakdownResponse {
  generatedAt: string
  timezone: string
  windowKey: string
  statusBreakdown: UsageBreakdownItem[]
  usageSourceBreakdown: UsageBreakdownItem[]
}

export interface UsageDashboardExternalPoolBillingResponse {
  generatedAt: string
  timezone: string
  windowKey: string
  externalPoolBillingByPool: UsageExternalPoolBillingByPool[]
}

export interface UsageDashboardWindow {
  key: string
  label: string
  from: string
  to: string
  summary: UsageDashboardSummary
}

export interface UsageDashboardSummary {
  totalRequests: number
  successRequests: number
  errorRequests: number
  errorRate: number
  streamRequests: number
  nonStreamRequests: number
  highCacheRequests: number
  totalInputTokens: number
  billableInputTokens: number
  totalOutputTokens: number
  totalCacheReadInputTokens: number
  totalCacheCreationInputTokens: number
  cacheReadRatio: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
  pricedRequests: number
  unpricedRequests: number
  averageDurationMs: number
  p95DurationMs: number
  stickyBoundRequests: number
  fallbackFromStickyRequests: number
  simulatedRequests: number
  upstreamMetadataRequests: number
  externalPoolBilling?: UsageExternalPoolBillingSummary
  externalPoolBillingByPool?: UsageExternalPoolBillingByPool[]
  statusBreakdown: UsageBreakdownItem[]
  usageSourceBreakdown: UsageBreakdownItem[]
}

export interface UsageBreakdownItem {
  key: string
  label: string
  requests: number
  ratio: number
}

export interface UsageDashboardSeries {
  hourly24h: UsageSeriesPoint[]
  daily7d: UsageSeriesPoint[]
}

export interface UsageSeriesPoint {
  key: string
  label: string
  from: string
  to: string
  requests: number
  successRequests: number
  errorRequests: number
  totalInputTokens: number
  billableInputTokens: number
  totalOutputTokens: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
}

export interface UsageDashboardTop {
  windowKey: string
  models: UsageTopAggregate[]
  credentials: UsageTopAggregate[]
  endpoints: UsageTopAggregate[]
  errors: UsageTopAggregate[]
}

export interface UsageTopAggregate {
  key: string
  label?: string
  requests: number
  errorRequests: number
  totalInputTokens: number
  billableInputTokens: number
  totalOutputTokens: number
  totalCacheReadInputTokens: number
  totalCacheCreationInputTokens: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
}

export interface UsageRecordsQuery {
  limit?: number
  requestId?: string
  q?: string
  endpoint?: string
  conversationId?: string
  credentialId?: number
  externalPoolId?: number
  routeKind?: UsageRouteKindFilter
  model?: string
  status?: UsageRecordStatus
  source?: UsageSource
  stream?: boolean
  minCacheRead?: number
  minFirstTokenLatencyMs?: number
  since?: string
  until?: string
}

export interface UsageRecordsPageQuery extends UsageRecordsQuery {
  page: number
  limit: number
}

export interface AdminAuditLogRow {
  id: number
  createdAt: string
  actor: string
  action: string
  objectType: string
  objectId?: string
  success: boolean
  errorMessage?: string
  detail: unknown
}

export interface AdminAuditLogPage {
  page: number
  limit: number
  hasNext: boolean
  records: AdminAuditLogRow[]
}

export interface AdminAuditLogPageQuery {
  page: number
  limit: number
}

export type CompatProfile = 'claude-code' | 'anthropic-strict' | 'debug'
export type KiroAgentModeStrategy = 'vibe' | 'spec' | 'auto'
export type ModelResolutionMode = 'compatible' | 'alias_only' | 'exact_only'
export type ThinkingTriggerMode = 'real_request' | 'always'
export type ModelMappingRuleKind = 'version_equivalent' | 'alias' | 'fallback'
export type PayloadGuardMode = 'preemptive' | 'on_too_long'
export type ExternalPoolAuthType = 'bearer' | 'x_api_key'
export type ExternalPoolUsageProjectionMode = 'pass_through' | 'current_path_policy'
export type ExternalPoolStreamResponseMode = 'event_passthrough'
export type ExternalPoolRequestBodyMode = 'normalized' | 'raw_passthrough'
export type ExternalPoolRawModelMode = 'none' | 'probe_only' | 'rewrite_top_level'
export type ExternalPoolAutoDisablePolicy = 'inherit' | 'disabled' | 'enabled'
export type ExternalPoolModelMappingMode = 'passthrough' | 'passthrough_mapping' | 'direct_mapping' | 'processed_mapping'

export interface ExternalPoolModelMappingRule {
  enabled?: boolean
  source: string
  target: string
  kind?: 'version_equivalent' | 'alias' | 'fallback'
  note?: string
}

export type ReportedUsageFieldMode = 'raw' | 'preserve' | 'sample-max' | 'sample-target'

export interface ModelMappingRule {
  enabled: boolean
  source: string
  target: string
  kind: ModelMappingRuleKind
  note?: string | null
}

export interface ModelMappingConfig {
  enabled: boolean
  autoGenerateRules: boolean
  rules: ModelMappingRule[]
}

export interface ReportedUsageFieldPolicy {
  mode: ReportedUsageFieldMode
  maxTokens: number
  targetTokens: number
  normalMaxMultiplier: number
  moveDeltaToCacheRead: boolean
}

export interface ReportedUsagePathPolicy {
  enabled: boolean
  skipNonStreamUsageProjection: boolean
  finalCacheReadMaxTokens: number
  finalCacheReadJitterMinTokens: number
  finalCacheReadJitterMaxTokens: number
  outputUpliftMinTokens: number
  outputUpliftPercent: number
  finalOutputMaxTokens: number
  finalOutputJitterMinTokens: number
  finalOutputJitterMaxTokens: number
  input: ReportedUsageFieldPolicy
  output: ReportedUsageFieldPolicy
  cacheRead: ReportedUsageFieldPolicy
  cacheCreation: ReportedUsageFieldPolicy
}

export interface ReportedUsageConfig {
  default: ReportedUsagePathPolicy
  pathOverrides: Record<string, ReportedUsagePathPolicy>
}

export interface CacheSimulationPolicyPatch {
  enabled?: boolean
  targetReadRatio?: number
  tokenScale?: number
  maxSimulatedInputTokens?: number
  capJitterMinTokens?: number
  capJitterMaxTokens?: number
  scaleMinInputTokens?: number
}

export interface CachePointPolicyPatch {
  enabled?: boolean
  toolsOnly?: boolean
  recordPlan?: boolean
}

export interface CacheBoundsPolicyPatch {
  maxEntriesPerAccount?: number
  maxEntriesGlobal?: number
  entryTtlSecs?: number
  estimatedBytesLimit?: number
}

export interface KiroRsToolCachePolicyPatch {
  coverageRatio?: number
  maxCoverageTokens?: number
  incrementalCreateEnabled?: boolean
  maxNewCreationTokensPerRequest?: number
  cacheCurrentUserStablePrefix?: boolean
  currentUserStablePrefixMaxTokens?: number
}

export type PromptCacheStrategyType = 'no_cache' | 'current_high_cache' | 'kiro_rs_tool'

export interface CacheRoutePolicyPatch {
  cacheType?: PromptCacheStrategyType
  simulation?: CacheSimulationPolicyPatch
  creationControl?: PromptCacheCreationControlConfig
  reportedUsage?: ReportedUsagePathPolicy
  cachePoint?: CachePointPolicyPatch
  bounds?: CacheBoundsPolicyPatch
  kiroRsTool?: KiroRsToolCachePolicyPatch
}

export interface CachePolicyConfig {
  default: CacheRoutePolicyPatch
  currentHighCache: CacheRoutePolicyPatch
  kiroRsTool: CacheRoutePolicyPatch
  pathOverrides: Record<string, CacheRoutePolicyPatch>
}

export interface PromptCacheCreationControlConfig {
  enabled: boolean
  scopeMode: 'credential_conversation_model' | 'conversation_model'
  minSuccessfulRequestsBetweenCreation: number
  minCreationIntervalSecs: number
  minCreationDeltaTokens: number
  maxCreationTokensPerEvent: number
  creationBudgetWindowSecs: number
  maxCreationTokensPerWindow: number
  expireAfterIdleSecs: number
}

export type OversizedImageHandling = 'drop-with-placeholder' | 'reject'
export type ImageProcessingMode = 'safe' | 'light'

export interface ImageProcessingConfig {
  mode: ImageProcessingMode
  safeMaterializeFileSources: boolean
  safeDownloadRemoteSources: boolean
  safeNormalizeBase64MediaTypes: boolean
}

export interface BodyConversionConfig {
  toolSchemaNormalization: boolean
  toolNameMapping: boolean
  toolSchemaKeyMapping: 'sanitize' | 'reject' | 'disabled'
  toolSchemaKeyValidationRegex: string
  toolChoiceSteering: boolean
  chunkedToolPolicy: boolean
  thinkingPromptControls: boolean
  nativeReasoningFields: boolean
  toolPairingRepair: boolean
  historyPlaceholderTools: boolean
}

export interface PayloadShapingConfig {
  enabled: boolean
  truncateHistoricalToolResults: boolean
  historicalToolResultMaxChars: number
  historicalToolResultHeadLines: number
  historicalToolResultTailLines: number
  discardHistoricalThinking: boolean
  compressToolDefinitions: boolean
  toolDefinitionsBudgetBytes: number
  toolDescriptionMaxChars: number
  toolSchemaAnnotationMaxChars: number
  webFetchTrimEnabled: boolean
  webFetchBodyMaxChars: number
  fitCurrentPayloadToBudget: boolean
  truncateCurrentToolResults: boolean
  currentToolResultMaxChars: number
  truncateCurrentUserContent: boolean
  currentUserContentMaxChars: number
  truncateCurrentDocuments: boolean
  currentDocumentMaxChars: number
  truncateCurrentImages: boolean
  currentImagesMaxBytes: number
  oversizedImageHandling: OversizedImageHandling
}

export interface ExternalPoolsConfig {
  externalPoolsEnabled: boolean
  externalPoolGlobalMaxConcurrentRequests: number
  externalPoolMaxQueuedRequests: number
  externalPoolMaxInputTokens: number
  externalPoolCapacityMode: 'fail_fast' | 'wait'
  externalPoolDispatchMaxWaitSecs: number
  externalPoolRetryMaxAttempts: number
  externalDirectPolicyEnabled: boolean
  directExternalOnLocalMaintenance: boolean
  directExternalModelRules: string[]
  directExternalPathRules: string[]
  fallbackOnLocalCapacityExhausted: boolean
  fallbackOnNoAvailableCredentials: boolean
  fallbackOnLocalTransientExhausted: boolean
  fallbackOnUnsupportedModel: boolean
  localPoolPreflightEnabled: boolean
  externalPoolLocalRescueEnabled: boolean
  externalPoolLocalRescueOnRateLimit: boolean
  externalPoolLocalRescueOnTimeout: boolean
  externalPoolLocalRescueOnCapacity: boolean
  externalPoolLocalRescueMaxWaitSecs: number
  localPoolCircuitEnabled: boolean
  localPoolCircuitWindowSecs: number
  localPoolCircuitOpenAfterFailures: number
  localPoolCircuitRequireDistinctCredentials: number
  localPoolCircuitOpenSecs: number
  externalPoolAutoDisableEnabled: boolean
  externalPoolAutoDisableOnAuthError: boolean
  externalPoolAutoDisableOnSecurityLock: boolean
  externalPoolAutoDisableOnQuotaExhausted: boolean
  externalPoolAutoDisableOnMisconfiguredEndpoint: boolean
  externalPoolAutoDisableFailureThreshold: number
  externalPoolAutoDisableWindowSecs: number
  externalPoolAutoDisableDurationSecs: number
  externalPoolRateLimitCooldownSecs: number
  externalPoolServerErrorCooldownSecs: number
  externalPoolNetworkErrorCooldownSecs: number
  externalPoolProtocolErrorCooldownSecs: number
  externalPoolRequestTimeoutSecs: number
  externalPoolStreamRequestTimeoutSecs: number
  externalPoolStreamIdleTimeoutSecs: number
  externalPoolAutoDisableOnChannelDisabled: boolean
  externalPoolUsageProjectionUpliftPercent: number
  externalPoolUsageProjectionOutputUpliftMinTokens: number
  externalPoolUsageProjectionOutputUpliftPercent: number
  externalPoolStreamResponseMode: ExternalPoolStreamResponseMode
}

export interface ExternalPool {
  id: number
  name: string
  baseUrl: string
  apiKey?: string
  maskedApiKey?: string
  authType: ExternalPoolAuthType
  enabled: boolean
  priority: number
  maxConcurrentRequests: number
  usageProjectionMode: ExternalPoolUsageProjectionMode
  streamResponseMode?: ExternalPoolStreamResponseMode
  requestBodyMode: ExternalPoolRequestBodyMode
  rawModelMode: ExternalPoolRawModelMode
  autoDisablePolicy: ExternalPoolAutoDisablePolicy
  autoDisabled: boolean
  autoDisabledReason?: string
  autoDisabledAt?: string
  autoDisabledUntil?: string
  autoDisabledLastError?: string
  preservePath: boolean
  normalizeModelVersionDots: boolean
  modelMappingMode: ExternalPoolModelMappingMode
  modelMappingRequireMatch: boolean
  modelMappingRules: ExternalPoolModelMappingRule[]
  supportedModels: string[]
  notes?: string
  createdAt: string
  updatedAt: string
}

export interface ExternalPoolsListResponse {
  pools: ExternalPool[]
}

export interface ExternalPoolStatus {
  pool: ExternalPool
  inFlight: number
  cooldownRemainingSecs: number
  cooldownReason?: string
  dispatchable: boolean
  skippedReason?: string
}

export interface ExternalPoolsStatusResponse {
  pools: ExternalPoolStatus[]
}

export interface CreateExternalPoolRequest {
  name: string
  baseUrl: string
  apiKey: string
  authType?: ExternalPoolAuthType
  enabled?: boolean
  priority?: number
  maxConcurrentRequests?: number
  usageProjectionMode?: ExternalPoolUsageProjectionMode
  streamResponseMode?: ExternalPoolStreamResponseMode | null
  requestBodyMode?: ExternalPoolRequestBodyMode
  rawModelMode?: ExternalPoolRawModelMode
  autoDisablePolicy?: ExternalPoolAutoDisablePolicy
  preservePath?: boolean
  normalizeModelVersionDots?: boolean
  modelMappingMode?: ExternalPoolModelMappingMode
  modelMappingRequireMatch?: boolean
  modelMappingRules?: ExternalPoolModelMappingRule[]
  supportedModels?: string[]
  notes?: string
}

export interface UpdateExternalPoolRequest {
  name?: string
  baseUrl?: string
  apiKey?: string
  authType?: ExternalPoolAuthType
  enabled?: boolean
  priority?: number
  maxConcurrentRequests?: number
  usageProjectionMode?: ExternalPoolUsageProjectionMode
  streamResponseMode?: ExternalPoolStreamResponseMode | null
  requestBodyMode?: ExternalPoolRequestBodyMode
  rawModelMode?: ExternalPoolRawModelMode
  autoDisablePolicy?: ExternalPoolAutoDisablePolicy
  preservePath?: boolean
  normalizeModelVersionDots?: boolean
  modelMappingMode?: ExternalPoolModelMappingMode
  modelMappingRequireMatch?: boolean
  modelMappingRules?: ExternalPoolModelMappingRule[]
  supportedModels?: string[]
  notes?: string
}

export interface SetSupportedModelsRequest {
  supportedModels: string[]
}

export interface DiscoverExternalPoolSupportedModelsRequest {
  baseUrl?: string | null
  apiKey?: string | null
  authType?: 'bearer' | 'x_api_key' | null
}

export interface SupportedModelsResponse {
  supportedModels: string[]
  count: number
}

export interface ExternalPoolTestResponse {
  ok: boolean
  status?: number
  message: string
  model?: string
  response?: string
}

export interface ExternalPoolTestRequest {
  model: string
  prompt?: string
}

export interface WeightedCapacityTier {
  minTokens: number
  units: number
}

export interface WeightedCapacityConfig {
  enabled: boolean
  maxUnitsPerRequest: number
  tiers: WeightedCapacityTier[]
}

export type MissingMaxTokensPolicy = 'reject' | 'default_value'

export interface MissingMaxTokensConfig {
  policy: MissingMaxTokensPolicy
  defaultValue: number
}

export interface RuntimeConfig {
  proxyUrl?: string | null
  proxyUsername?: string | null
  proxyPassword?: string | null
  credentialRpm: number
  credentialMaxConcurrentRequests: number
  credentialTransientCooldownSecs: number
  credentialRateLimitCooldownSecs: number
  credentialServerErrorCooldownSecs: number
  credentialNetworkErrorCooldownSecs: number
  credentialStreamErrorCooldownSecs: number
  credentialProtocolErrorCooldownSecs: number
  credentialAuthErrorCooldownSecs: number
  credentialCooldownBackoffMultiplier: number
  credentialCooldownJitterPercent: number
  credentialProbationSecs: number
  credentialMaxCooldownSecs: number
  credentialDispatchMaxWaitSecs: number
  kiroUpstreamResponseTimeoutSecs: number
  kiroUpstreamStreamIdleTimeoutSecs: number
  credentialRetryMaxAttempts: number
  credentialPromptLogicRetryEnabled: boolean
  credentialPromptLogicRetryMaxAttempts: number
  credentialInFlightLeaseMaxSecs: number
  dispatchGlobalMaxConcurrentRequests: number
  dispatchMaxQueuedRequests: number
  weightedCapacity: WeightedCapacityConfig
  credentialWarmupRequests: number
  credentialWarmupSelectionPercent: number
  credentialWarmupMaxSelectionPercent: number
  schedulerErrorEwmaAlpha: number
  schedulerPriorityWeight: number
  schedulerLoadWeight: number
  schedulerErrorWeight: number
  schedulerLatencyWeight: number
  schedulerProbationWeight: number
  schedulerSelectionPressureWeight: number
  schedulerTotalSelectionWeight: number
  schedulerTopK: number
  selectionFailureSampleLimit: number
  selectionFailureRecordEnabled: boolean
  compressionEnabled: boolean
  whitespaceCompression: boolean
  imageProcessing: ImageProcessingConfig
  bodyConversion: BodyConversionConfig
  missingMaxTokens: MissingMaxTokensConfig
  payloadGuardEnabled: boolean
  payloadGuardMode: PayloadGuardMode
  payloadGuardMaxBytes: number
  payloadGuardSafetyMarginBytes: number
  payloadGuardTrimHistory: boolean
  payloadGuardExternalEnabled: boolean
  kiroCachePointEnabled: boolean
  kiroCachePointToolsOnly: boolean
  kiroCachePointRecordPlan: boolean
  payloadShaping: PayloadShapingConfig
  promptCacheTargetReadRatio: number
  promptCacheTokenScale: number
  promptCacheMaxSimulatedInputTokens: number
  promptCacheCapJitterMinTokens: number
  promptCacheCapJitterMaxTokens: number
  promptCacheScaleMinInputTokens: number
  promptCacheCreationControl: PromptCacheCreationControlConfig
  promptCacheMaxEntriesPerAccount: number
  promptCacheMaxEntriesGlobal: number
  promptCacheEntryTtlSecs: number
  promptCacheEstimatedBytesLimit: number
  reportedUsage: ReportedUsageConfig
  cachePolicy: CachePolicyConfig
  externalPools: ExternalPoolsConfig
  highCacheThreshold: number
  compatProfile: CompatProfile
  kiroAgentModeStrategy: KiroAgentModeStrategy
  modelResolutionMode: ModelResolutionMode
  modelMapping: ModelMappingConfig
  extractThinking: boolean
  thinkingTriggerMode: ThinkingTriggerMode
  exposeProxyWarnings: boolean
  definedCacheRoutes: string[]
}

export type UpdateRuntimeConfigRequest = RuntimeConfig

export interface AccessKeysResponse {
  requestApiKey: string
  maskedRequestApiKey: string
  requestApiKeys: RequestApiKeyItem[]
  adminApiKey: string
  maskedAdminApiKey: string
}

export interface RequestApiKeyItem {
  id: string
  apiKey: string
  maskedApiKey: string
  primary: boolean
}

export interface CreateRequestApiKeyRequest {
  apiKey?: string
}

export interface UpdateRequestApiKeyRequest {
  apiKey?: string
}

export interface UpdateAdminApiKeyRequest {
  adminApiKey: string
}

export type LoadBalancingMode = 'priority' | 'balanced' | 'health_balanced' | 'weighted_least_inflight'

export interface ModelPricing {
  inputCostPerToken: number
  outputCostPerToken: number
  cacheCreationInputTokenCost: number
  cacheReadInputTokenCost: number
}

export interface ModelPriceItem {
  model: string
  pricing: ModelPricing
  source?: string
}

export interface ModelPricingStatus {
  available: boolean
  source: string
  sourceUrl: string
  modelCount: number
  lastSyncedAt?: string
  lastError?: string
  models: ModelPriceItem[]
}

export interface ModelCapabilityItem {
  model: string
  displayName: string
  description?: string
  maxInputTokens?: number
  maxOutputTokens?: number
  supportsPromptCaching?: boolean
  supportedInputTypes: string[]
  source?: string
}

export interface ModelCapabilitiesStatus {
  available: boolean
  source: string
  modelCount: number
  lastSyncedAt?: string
  lastError?: string
  models: ModelCapabilityItem[]
}

export interface ManualModelPricingRequest {
  inputCostPerMillion: number
  outputCostPerMillion: number
  cacheCreationInputCostPerMillion?: number
  cacheReadInputCostPerMillion?: number
}

export interface UpsertManualModelRequest {
  model: string
  displayName?: string
  description?: string
  maxInputTokens?: number
  maxOutputTokens?: number
  supportsPromptCaching?: boolean
  supportedInputTypes: string[]
  pricing?: ManualModelPricingRequest
  clearPricing?: boolean
}

export interface ManualModelResponse {
  success: boolean
  message: string
  model: string
}

export type CredentialExportFormat = 'json' | 'backup-json' | 'jsonl'
