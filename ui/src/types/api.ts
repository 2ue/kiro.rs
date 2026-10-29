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
  normal: number
  cooling: number
  quotaExhausted: number
  accountError: number
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
  /** Fast 值：0 表示未开启，数字越大越先打满。 */
  fast: number
  /** 路由调度规则，语义与外部池相同；allow_all 表示不限制。 */
  routeMode: ExternalPoolRouteMode
  routeRules: string[]
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
  | 'fast'
  | 'routeMode'
  | 'routeRules'
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

export interface BatchDeleteCredentialsRequest {
  ids: number[]
  disableFirst?: boolean
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

export type UsageCleanupMode = 'soft_delete' | 'hard_delete'
export type UsageCleanupJobStatus = 'idle' | 'queued' | 'running' | 'paused' | 'completed' | 'cancelled' | 'failed'
export type UsageCleanupPhase = 'idle' | 'postgres' | 'redis_admin_cache' | 'redis_snapshots' | 'complete'

export interface UsageCleanupRequest {
  mode?: UsageCleanupMode
  includeSummary?: boolean
  olderThanDays?: number
  cutoffBefore?: string
  batchSize?: number
  maxBatches?: number
  maxRows?: number
  pauseMsBetweenBatches?: number
}

export interface UsageCleanupPreviewResponse {
  mode: UsageCleanupMode
  includeSummary: boolean
  cutoffAt: string
  matchedRows: number
  oldestCreatedAt?: string
  newestCreatedAt?: string
}

export interface UsageCleanupStatusResponse {
  jobId?: string
  status: UsageCleanupJobStatus
  phase: UsageCleanupPhase
  mode?: UsageCleanupMode
  includeSummary: boolean
  cutoffAt?: string
  batchSize: number
  maxBatches: number
  maxRows?: number
  pauseMsBetweenBatches: number
  matchedRows?: number
  remainingRows?: number
  processedRows: number
  lastBatchRows: number
  batches: number
  redisDeletedKeys: number
  redisDeleteCommands: number
  redisMaxCommandKeys: number
  redisScanPasses: number
  redisUsedDelFallback: boolean
  redisPassLimitReached: boolean
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

export interface SetCredentialFastRequest {
  fast: number
}

export interface SetCredentialRoutePolicyRequest {
  routeMode: ExternalPoolRouteMode
  routeRules: string[]
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
  fast?: SetCredentialFastRequest
  routePolicy?: SetCredentialRoutePolicyRequest
  regions?: SetCredentialRegionsRequest
  concurrency?: SetCredentialConcurrencyRequest
  rpm?: SetCredentialRpmRequest
  rateLimitAutoDisable?: SetCredentialRateLimitAutoDisableRequest
  proxy?: SetCredentialProxyRequest
  /** Proxy resources assigned in turn in the order of `ids`; replaces `proxy` when non-empty. */
  proxyResourceIds?: number[]
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
  lastTestAt?: string | null
  lastTestSuccess?: boolean | null
  lastTestStatus?: number | null
  lastTestDurationMs?: number | null
  lastTestMessage?: string | null
  lastTestUrl?: string | null
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

export interface ProxyResourceBatchTestResponse {
  total: number
  success: number
  failed: number
  items: Array<ProxyResourceTestResponse & { resourceId: number; name: string }>
}

export interface CreateProxyResourceRequest {
  name: string
  proxyUrl: string
  proxyUsername?: string
  proxyPassword?: string
  enabled?: boolean
  notes?: string
}

export interface ProxyResourceImportEntry {
  name?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  enabled?: boolean
  notes?: string
}

export interface BatchProxyResourceImportRequest {
  content?: string
  resources?: ProxyResourceImportEntry[]
  namePrefix?: string
  defaultProtocol?: string
  enabled?: boolean
  deduplicate?: boolean
  continueOnError?: boolean
}

export interface BatchProxyResourceImportItem {
  index: number
  ok: boolean
  skipped?: boolean
  resourceId?: number
  name?: string
  proxyUrl?: string
  error?: string
  warning?: string
}

export interface BatchProxyResourceImportResponse {
  total: number
  success: number
  skipped: number
  failed: number
  items: BatchProxyResourceImportItem[]
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

export interface InferenceAttemptSnapshot {
  maxAttempts: number
  consumed: number
  localAttempts: number
  externalAttempts: number
  mcpAttempts: number
  exhausted: boolean
  downstreamCommitted: boolean
}

export interface AuxiliaryAttemptSnapshot {
  maxAttempts: number
  consumed: number
  tokenRefreshAttempts: number
  profileDiscoveryAttempts: number
  exhausted: boolean
}

export interface UsageLatencyTrace {
  inferenceAttempts?: InferenceAttemptSnapshot
  auxiliaryAttempts?: AuxiliaryAttemptSnapshot
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
  streamRetryAttempts?: number
  streamRetryDispatchFailures?: number
  streamRetryReasons?: string[]
  clientDroppedMs?: number
  terminalReason?: 'completed' | 'upstream_status_error' | 'upstream_json_exception' | 'upstream_idle_timeout' | 'first_output_timeout' | 'malformed_sse' | 'client_dropped' | 'internal_error'
  upstreamMessageStatus?: string
  sawUpstreamCompleted?: boolean
  stopReasonSource?: string
  suspectedIntentPreambleEndTurn?: boolean
  intentPreambleRisk?: 'none' | 'low' | 'medium' | 'high' | string
  suspectedToolContextLeakEndTurn?: boolean
  toolContextLeakMarkers?: string[]
  assistantTailIntentHint?: boolean
  endTurnAnomalyReason?: string
  endTurnAnomalyRisk?: 'none' | 'low' | 'medium' | 'high' | string
  upstreamEofWithoutCompleted?: boolean
  lastUpstreamEventType?: string
  lastUpstreamEvents?: string[]
  sawUpstreamAssistantResponse?: boolean
  sawUpstreamToolUse?: boolean
  sawUpstreamMetadata?: boolean
  lastAssistantContentChars?: number
  filteredTrivialTextBlocks?: number
  filteredTrivialTextChars?: number
  websearchMode?: 'template' | 'model_driven' | string
  webSearchRequests?: number
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
  rawUpstreamError?: RawUpstreamError
  durationMs: number
}

export interface RawUpstreamError {
  source: string
  statusCode?: number
  contentType?: string
  body: string
  bodyBytes: number
  truncated: boolean
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
  requestApiKeyId?: string
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
  routeSubtype?: 'local_success' | 'local_error_no_fallback' | 'local_rescue_after_external' | 'external_fallback_preflight' | 'external_fallback_after_local_attempts' | 'external_direct_policy' | 'external_route_policy' | 'external_error'
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
  rawUpstreamError?: RawUpstreamError
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
  usageEstimated?: boolean
  usageEstimateReason?: string
  usageCandidatePath?: string
  bodyUsageProjectionApplied?: boolean
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
  rawUpstreamError?: RawUpstreamError
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
  successRequests?: number
  errorRequests?: number
  rpm: number
  successRpm?: number
  errorRpm?: number
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
  totalKiroMeteringUsage: number
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

export interface UsageOverviewMetrics {
  requests: number
  successRequests: number
  errorRequests: number
  errorRate: number
  inputTokens: number
  /** 未缓存输入；缓存命中率 = 缓存读 / (未缓存输入 + 缓存读 + 缓存写)，与 sub2api 口径一致 */
  uncachedInputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage: number
  pricedRequests: number
  unpricedRequests: number
  averageDurationMs: number
}

export interface UsageOverviewSeriesMetrics {
  requests: number
  errorRequests: number
  inputTokens: number
  uncachedInputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage: number
  /** 外部池上游原始成本（只有外部池记录有值） */
  rawCostUsd: number
  /** 外部池最终可计费金额 */
  billableCostUsd: number
  /** 外部池补偿后成本 - 上游原始成本 */
  profitUsd: number
}

export interface UsageOverviewSeriesBreakdown {
  key: string
  metrics: UsageOverviewSeriesMetrics
}

export interface UsageOverviewSeriesPoint {
  bucketStart: string
  label: string
  all: UsageOverviewSeriesMetrics
  local: UsageOverviewSeriesMetrics
  external: UsageOverviewSeriesMetrics
  localRoutes: UsageOverviewSeriesBreakdown[]
  externalPools: UsageOverviewSeriesBreakdown[]
  externalReasons: UsageOverviewSeriesBreakdown[]
}

export interface UsageOverviewSeriesGroup {
  key: string
  label: string
}

export interface UsageOverviewSeriesGroups {
  /** 按消耗积分降序 */
  localRoutes: UsageOverviewSeriesGroup[]
  /** 按上游原始成本降序 */
  externalPools: UsageOverviewSeriesGroup[]
  /** 按请求数降序 */
  externalReasons: UsageOverviewSeriesGroup[]
}

export interface UsageOverviewPoolRow {
  poolId: number
  poolName: string
  metrics: UsageOverviewMetrics
  rawCostUsd: number
  shapedCostUsd: number
  upliftedCostUsd: number
  reportedCostUsd: number
  billableCostUsd: number
  profitUsd: number
  costFloorDeltaUsd: number
  costFloorAppliedRequests: number
}

export interface UsageOverviewAccountRow {
  credentialId: number
  label: string
  metrics: UsageOverviewMetrics
}

export interface UsageOverviewRouteRow {
  key: string
  label: string
  metrics: UsageOverviewMetrics
}

export interface UsageOverviewRankRow {
  key: string
  label: string
  requests: number
  errorRequests: number
  estimatedCostUsd: number
}

export interface UsageOverviewResponse {
  generatedAt: string
  timezone: string
  range: {
    key?: string | null
    requestedFrom: string
    requestedTo: string
    from: string
    to: string
    dataThrough: string
    granularity: 'hour' | 'day' | string
  }
  totals: {
    all: UsageOverviewMetrics
    local: UsageOverviewMetrics
    external: UsageOverviewMetrics
  }
  external: {
    rawCostUsd: number
    shapedCostUsd: number
    upliftedCostUsd: number
    reportedCostUsd: number
    billableCostUsd: number
    profitUsd: number
    costFloorDeltaUsd: number
    costFloorAppliedRequests: number
    pools: UsageOverviewPoolRow[]
  }
  local: {
    activeAccounts: number
    accountsTotal: number
    accounts: UsageOverviewAccountRow[]
    routes: UsageOverviewRouteRow[]
  }
  series: UsageOverviewSeriesPoint[]
  seriesGroups: UsageOverviewSeriesGroups
  topModels: UsageOverviewRankRow[]
  topAccounts: UsageOverviewRankRow[]
  topKeys: UsageOverviewRankRow[]
  topPaths: UsageOverviewRankRow[]
  topErrors: UsageOverviewRankRow[]
}

export interface UsageOverviewSummaryResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  totals: UsageOverviewResponse['totals']
  external: {
    rawCostUsd: number
    shapedCostUsd: number
    upliftedCostUsd: number
    reportedCostUsd: number
    billableCostUsd: number
    profitUsd: number
    costFloorDeltaUsd: number
    costFloorAppliedRequests: number
  }
  local: {
    activeAccounts: number
    accountsTotal: number
  }
}

export interface UsageOverviewSeriesResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  series: UsageOverviewSeriesPoint[]
  seriesGroups: UsageOverviewSeriesGroups
}

export interface UsageOverviewLocalResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  local: UsageOverviewResponse['local']
}

export interface UsageOverviewExternalResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  external: UsageOverviewResponse['external']
}

export interface UsageOverviewRankingsResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  totalRequests: number
  totalLocalRequests: number
  totalErrors: number
  topModels: UsageOverviewRankRow[]
  topAccounts: UsageOverviewRankRow[]
  topKeys: UsageOverviewRankRow[]
  topPaths: UsageOverviewRankRow[]
  topErrors: UsageOverviewRankRow[]
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

export interface UsageExternalPoolRiskQuery {
  timezone?: string
  windowKey?: string
  since?: string
  until?: string
  warningThresholdTokens?: number
  criticalThresholdTokens?: number
  externalPoolId?: number
  endpoint?: string
  model?: string
  stream?: boolean
  limit?: number
}

export interface UsageExternalPoolRiskWindow {
  key: string
  label: string
  from: string
  to: string
}

export interface UsageExternalPoolRiskThresholds {
  warningTokens: number
  criticalTokens: number
  costFloorEnabled: boolean
  costFloorMarginPercent: number
  costTargetMultiplier: number
}

export interface UsageExternalPoolRiskFilters {
  poolId?: number
  endpoint?: string
  model?: string
  stream?: boolean
}

export interface UsageExternalPoolRiskTotals {
  records: number
  successRecords: number
  errorRecords: number
  streamRecords: number
  nonStreamRecords: number
  pricedRecords: number
  unpricedRecords: number
  rawUsageRecords: number
  reportedUsageRecords: number
  missingExternalPoolBillingRecords: number
  outputZeroRecords: number
}

export interface UsageExternalPoolRiskCacheStats {
  minReadTokens: number
  maxReadTokens: number
  avgReadTokens: number
  totalReadTokens: number
  minWriteTokens: number
  maxWriteTokens: number
  avgWriteTokens: number
  totalWriteTokens: number
  readWarningCount: number
  writeWarningCount: number
  eitherWarningCount: number
  readCriticalCount: number
  writeCriticalCount: number
  eitherCriticalCount: number
}

export interface UsageExternalPoolRiskCostStats {
  rawCostUsd: number
  reportedCostUsd: number
  targetCostUsd: number
  profitUsd: number
  totalLossUsd: number
  totalTargetGapUsd: number
  maxLossUsd: number
  maxTargetGapUsd: number
  maxRawCostUsd: number
  maxReportedCostUsd: number
  belowRawCount: number
  belowTargetCount: number
  costFloorAppliedRecords: number
  minCostRatio?: number | null
  avgCostRatio?: number | null
  maxCostRatio?: number | null
}

export interface UsageExternalPoolRiskBucket {
  key: string
  label: string
  minTokens?: number | null
  maxTokens?: number | null
  rawReadCount: number
  rawWriteCount: number
  reportedReadCount: number
  reportedWriteCount: number
}

export interface UsageExternalPoolRiskGroup {
  key: string
  label: string
  records: number
  successRecords: number
  warningRecords: number
  criticalRecords: number
  outputZeroRecords: number
  rawReadMax: number
  rawWriteMax: number
  reportedReadMax: number
  reportedWriteMax: number
  rawCostUsd: number
  reportedCostUsd: number
  targetCostUsd: number
  profitUsd: number
  totalLossUsd: number
  totalTargetGapUsd: number
  belowRawCount: number
  belowTargetCount: number
}

export interface UsageExternalPoolRiskSample {
  id: string
  createdAt: string
  endpoint: string
  stream: boolean
  model: string
  status: string
  externalPoolId?: number
  externalPoolName?: string
  pricingModel?: string
  usageProjectionMode?: string
  externalPoolBillingPresent: boolean
  costFloorApplied: boolean
  rawInputTokens: number
  rawOutputTokens: number
  rawCacheReadInputTokens: number
  rawCacheCreationInputTokens: number
  reportedInputTokens: number
  reportedOutputTokens: number
  reportedCacheReadInputTokens: number
  reportedCacheCreationInputTokens: number
  rawCostUsd: number
  reportedCostUsd: number
  targetCostUsd: number
  lossUsd: number
  targetGapUsd: number
  costRatio?: number | null
  riskReasons: string[]
}

export interface UsageExternalPoolRiskResponse {
  generatedAt: string
  timezone: string
  window: UsageExternalPoolRiskWindow
  thresholds: UsageExternalPoolRiskThresholds
  filters: UsageExternalPoolRiskFilters
  totals: UsageExternalPoolRiskTotals
  rawCache: UsageExternalPoolRiskCacheStats
  reportedCache: UsageExternalPoolRiskCacheStats
  cost: UsageExternalPoolRiskCostStats
  buckets: UsageExternalPoolRiskBucket[]
  byPool: UsageExternalPoolRiskGroup[]
  byPath: UsageExternalPoolRiskGroup[]
  byModel: UsageExternalPoolRiskGroup[]
  samples: UsageExternalPoolRiskSample[]
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
  totalKiroMeteringUsage: number
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
  totalKiroMeteringUsage: number
}

export interface UsageDashboardTop {
  windowKey: string
  models: UsageTopAggregate[]
  credentials: UsageTopAggregate[]
  endpoints: UsageTopAggregate[]
  errors: UsageTopAggregate[]
  modelsTotal: number
  credentialsTotal: number
  endpointsTotal: number
  errorsTotal: number
  modelsTruncated: boolean
  credentialsTruncated: boolean
  endpointsTruncated: boolean
  errorsTruncated: boolean
  orderBy: string
  errorsOrderBy: string
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
  totalKiroMeteringUsage: number
}

export interface UsageRecordsQuery {
  limit?: number
  requestId?: string
  requestApiKeyId?: string
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
  /** Only account change events. */
  accountEvents?: boolean
  accountEvent?: string
  accountSource?: string
  objectType?: string
  objectId?: string
  /** Account label substring or id. */
  account?: string
  action?: string
  actor?: string
  success?: boolean
  /** RFC3339, inclusive. */
  since?: string
  /** RFC3339, exclusive. */
  until?: string
}

export type AccountEventKind =
  | 'quota_exhausted'
  | 'quota_recovered'
  | 'overage_started'
  | 'auto_disabled'
  | 'risk_controlled'
  | 'auto_enabled'
  | 'manual_disabled'
  | 'manual_enabled'
  | 'deleted'

export interface AccountEventSummaryRow {
  event: AccountEventKind | string
  events: number
  accounts: number
}

export interface AccountEventSummaryResponse {
  hours: number
  since: string
  accountSource?: string | null
  items: AccountEventSummaryRow[]
}

export type CompatProfile = 'claude-code' | 'anthropic-strict' | 'debug'
export type KiroAgentModeStrategy = 'vibe' | 'spec' | 'auto'
export type ModelResolutionMode = 'compatible' | 'alias_only' | 'exact_only'
export type ThinkingTriggerMode = 'real_request' | 'always' | 'off'
export type ModelMappingRuleKind = 'version_equivalent' | 'alias' | 'fallback'
export type PayloadGuardMode = 'preemptive' | 'on_too_long'
export type ExternalPoolAuthType = 'bearer' | 'x_api_key'
export type ExternalPoolHeaderProfile = 'generic' | 'anthropic_passthrough' | 'claude_code_mimic'
export type ExternalPoolWireProfile = 'default' | 'http1_title_case'
export type ExternalPoolTlsProfile = 'default' | 'native_tls'
export type ExternalPoolUsageProjectionMode = 'pass_through' | 'current_path_policy'
export type ExternalPoolStreamResponseMode = 'event_passthrough'
export type ExternalPoolStreamRetryMode = 'inherit' | 'enabled' | 'disabled'
export type ExternalPoolModelUnavailableCooldownMode = 'disabled' | 'model' | 'pool'
export type ExternalPoolRequestBodyMode = 'normalized' | 'raw_passthrough'
export type ExternalPoolRawModelMode = 'none' | 'probe_only' | 'rewrite_top_level'
export type ExternalPoolAutoDisablePolicy = 'inherit' | 'disabled' | 'enabled'
export type ExternalPoolModelMappingMode = 'passthrough' | 'passthrough_mapping' | 'direct_mapping' | 'processed_mapping'
export type ExternalPoolRouteMode = 'allow_all' | 'allow_list' | 'deny_list'

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
  allowVersionFallback: boolean
  allowCrossFamilyFallback: boolean
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
  finalCacheCreationMaxTokens: number
  finalCacheCreationJitterMinTokens: number
  finalCacheCreationJitterMaxTokens: number
  finalOutputGuardEnabled: boolean
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

/** Kiro-RS Tool Plus：单个 usage 字段的放大、浮动与上下限。 */
export interface UsageFieldShaping {
  multiplier?: number
  jitterRatio?: number
  minTokens?: number
  maxTokens?: number
  maxJitterTokens?: number
}

export interface UsageShapingProfile {
  input?: UsageFieldShaping
  output?: UsageFieldShaping
  cacheRead?: UsageFieldShaping
  cacheCreation?: UsageFieldShaping
  capOverflowToInput?: boolean
  minCostRatio?: number
}

export type UsageShapingProfileMode = 'inherit' | 'custom' | 'raw'

export interface KiroRsToolPlusCachePolicy {
  coverageRatio?: number
  maxCoverageTokens?: number
  incrementalCreateEnabled?: boolean
  cacheCurrentUserStablePrefix?: boolean
  currentUserStablePrefixMaxTokens?: number
  creationFrequency?: PromptCacheCreationControlConfig
  suppressedCreationTarget?: 'input' | 'cache_read' | 'drop'
  shaping?: UsageShapingProfile
  nonStream?: { mode?: UsageShapingProfileMode; profile?: UsageShapingProfile }
  smallRequest?: {
    enabled?: boolean
    maxInputTokens?: number
    disableCache?: boolean
    mode?: UsageShapingProfileMode
    profile?: UsageShapingProfile
  }
}

export interface KiroRsToolCachePolicyPatch {
  coverageRatio?: number
  maxCoverageTokens?: number
  incrementalCreateEnabled?: boolean
  maxNewCreationTokensPerRequest?: number
  cacheCurrentUserStablePrefix?: boolean
  currentUserStablePrefixMaxTokens?: number
}

export interface StableSegmentCachePolicyPatch {
  coverageRatio?: number
  maxCoverageTokens?: number
  minTotalInputTokens?: number
  minSegmentTokens?: number
  incrementalCreateEnabled?: boolean
  maxNewCreationTokensPerRequest?: number
  includeTools?: boolean
  includeSystem?: boolean
  includeHistory?: boolean
  historyMessageLimit?: number
  honorExplicitCacheControl?: boolean
  autoCacheSystem?: boolean
  autoCacheTools?: boolean
  autoCacheHistoryMessageEnds?: boolean
  cacheCurrentUserStablePrefix?: boolean
  currentUserStablePrefixMaxTokens?: number
  defaultTtlSecs?: number
  extendedTtlSecs?: number
  reportedInputMinTokens?: number
  reportedInputMaxTokens?: number
  tokenScale?: number
  maxSimulatedInputTokens?: number
  capJitterMinTokens?: number
  capJitterMaxTokens?: number
  scaleMinInputTokens?: number
  outputUpliftPercent?: number
  outputUpliftMinTokens?: number
  finalCacheReadMaxTokens?: number
  finalCacheReadJitterMinTokens?: number
  finalCacheReadJitterMaxTokens?: number
  finalCacheCreationMaxTokens?: number
  finalCacheCreationJitterMinTokens?: number
  finalCacheCreationJitterMaxTokens?: number
  finalOutputMaxTokens?: number
  finalOutputJitterMinTokens?: number
  finalOutputJitterMaxTokens?: number
}

export type PromptCacheStrategyType = 'no_cache' | 'current_high_cache' | 'kiro_rs_tool' | 'stable_segment_cache' | 'kiro_rs_tool_plus' | 'bounded_high_cache'

export interface CacheRoutePolicyPatch {
  cacheType?: PromptCacheStrategyType
  routeNamespace?: boolean
  simulation?: CacheSimulationPolicyPatch
  creationControl?: PromptCacheCreationControlConfig
  reportedUsage?: ReportedUsagePathPolicy
  cachePoint?: CachePointPolicyPatch
  bounds?: CacheBoundsPolicyPatch
  kiroRsTool?: KiroRsToolCachePolicyPatch
  kiroRsToolPlus?: KiroRsToolPlusCachePolicy
  stableSegment?: StableSegmentCachePolicyPatch
  hitShaping?: CacheHitShapingPolicyPatch
  coldRequestGuard?: ColdRequestGuardPolicyPatch
  cacheTtl?: CacheTtlPolicyPatch
}

/**
 * 缓存写入时长：最终 usage 的写入全部记为 5 分钟（ephemeral_5m，普通缓存）或 1 小时（ephemeral_1h）。
 * "请求标记"指 cache_control.ttl；只写 cache_control 不写 ttl 视为未指定。
 * upstream_then_request_default_5m 只用于外部池。
 */
export type CacheTtlMode =
  | 'follow_request_default_5m'
  | 'follow_request_default_1h'
  | 'force_5m'
  | 'force_1h'
  | 'upstream_then_request_default_5m'

export interface CacheTtlPolicyPatch {
  local?: CacheTtlMode
  externalPool?: CacheTtlMode
}

/** 冷请求 usage 约束：默认关闭，关闭时与未引入该约束的行为一致。 */
export interface ColdRequestGuardPolicyPatch {
  enabled?: boolean
}

export interface CacheHitShapingPolicyPatch {
  enabled?: boolean
  targetHitRatio?: number
  ratioJitter?: number
  minCacheReadTokens?: number
  minCacheCreationTokens?: number
  minCostRatio?: number
  maxReadMultiplier?: number
  minCreationRatio?: number
}

export interface CachePolicyConfig {
  default: CacheRoutePolicyPatch
  currentHighCache: CacheRoutePolicyPatch
  kiroRsTool: CacheRoutePolicyPatch
  stableSegmentCache: CacheRoutePolicyPatch
  kiroRsToolPlus?: CacheRoutePolicyPatch
  /** 上下限优先缓存策略模板，不继承 default。 */
  boundedHighCache?: CacheRoutePolicyPatch
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
  /** 发往上游前压缩过大的图片（默认关闭）。 */
  resizeEnabled: boolean
  resizeMaxLongSide: number
  resizeMaxBytes: number
  resizeJpegQuality: number
}

export interface BodyConversionConfig {
  toolSchemaNormalization: boolean
  toolNameMapping: boolean
  toolSchemaKeyMapping: 'sanitize' | 'reject' | 'disabled'
  toolSchemaKeyValidationRegex: string
  /** Structured tool_choice handling: tool filtering and response-side enforcement only. */
  toolChoiceSteering: boolean
  nativeReasoningFields: boolean
  syntheticThinkingSignatures: boolean
  toolPairingRepair: boolean
  historyPlaceholderTools: boolean
  historyImageDedup: boolean
  codeExecution: CodeExecutionConfig
}

export interface CodeExecutionConfig {
  enabled: boolean
  pathOverrides: Record<string, boolean>
  timeoutMs: number
  maxOutputBytes: number
  maxCodeBytes: number
}

export type InjectionId =
  | 'taskQuality'
  | 'languageConstraint'
  | 'languageTurnReminder'
  | 'custom'
  | 'identityPrompt'
  | 'identityTurnReminder'
  | 'chunkedWritePolicy'
  | 'chunkedWriteToolSuffix'
  | 'thinkingControls'
  | 'thinkingOutputPolicy'
  | 'toolChoicePrefix'
  | 'toolChoiceTurnReminder'
  | 'webSearchGuidance'
  | 'emptyTurnNudge'

export type InjectionTextKey =
  | 'taskQuality'
  | 'languageConstraint'
  | 'languageTurnReminder.explicit'
  | 'languageTurnReminder.followUser'
  | 'custom'
  | 'identityPrompt'
  | 'identityTurnReminder'
  | 'chunkedWritePolicy'
  | 'chunkedWriteToolSuffix.write'
  | 'chunkedWriteToolSuffix.edit'
  | 'webSearchGuidance'
  | 'emptyTurnNudge'

export interface InjectionItemSetting {
  enabled: boolean
  /** Only present on items that can be injected into external-pool requests. */
  externalPool?: boolean
}

export interface InjectionItemOverride {
  enabled?: boolean
  externalPool?: boolean
}

export interface PromptInjectionConfig {
  applyToCountTokens: boolean
  defaults: Record<InjectionId, InjectionItemSetting>
  /** Edited texts only; a missing key uses the built-in text from the catalog. */
  texts: Partial<Record<InjectionTextKey, string>>
  pathOverrides: Record<string, Partial<Record<InjectionId, InjectionItemOverride>>>
}

export interface RouteFeatureSet {
  thinkingModelName: boolean
  thinkingTrigger: ThinkingTriggerMode
  channelLeakGuard: boolean
  channelLeakGuardStream: boolean
  modelDrivenWebSearch: boolean
}

export type RouteFeatureOverride = Partial<RouteFeatureSet>

export interface RouteFeaturesConfig {
  defaults: RouteFeatureSet
  pathOverrides: Record<string, RouteFeatureOverride>
}

export type InjectionCategory = 'operator' | 'protocol' | 'feature' | 'recovery'
export type InjectionRecommendation = 'keep_on' | 'user_choice'

export interface InjectionTextVariant {
  key: InjectionTextKey
  label: string
  defaultText: string
}

export interface InjectionCatalogEntry {
  id: InjectionId
  code: string
  name: string
  category: InjectionCategory
  position: string
  trigger: string
  dependsOn: string | null
  impactWhenOff: string
  recommendation: InjectionRecommendation
  recommendationNote: string
  defaultEnabled: boolean
  externalPool: boolean
  strictBlocks: boolean
  editable: boolean
  texts: InjectionTextVariant[]
  readOnlyVariants: InjectionTextVariant[]
}

export interface FixedInjection {
  code: string
  name: string
  text: string
  reason: string
}

export interface PromptInjectionCatalog {
  items: InjectionCatalogEntry[]
  fixed: FixedInjection[]
  maxTextChars: number
  maxTotalTextChars: number
}

export type PreviewToolChoice = 'auto' | 'any' | 'tool' | 'none'
export type PreviewThinking = 'off' | 'enabled' | 'adaptive'

export interface PromptInjectionPreviewRequest {
  route: string
  upstream?: 'local' | 'external_pool'
  model?: string
  nativeReasoning?: boolean
  withSystem?: boolean
  withWriteEdit?: boolean
  disableParallelToolUse?: boolean
  toolChoice?: PreviewToolChoice
  thinking?: PreviewThinking
  language?: 'none' | 'declared'
  emptyTurnRetry?: boolean
  stream?: boolean
  /** Unsaved settings used only for this preview. */
  draft?: { promptInjection?: PromptInjectionConfig; routeFeatures?: RouteFeaturesConfig }
}

export type PreviewBlockReason = 'off' | 'external_pool_unsupported' | 'external_pool_off' | 'strict'

export interface PromptInjectionPreviewItem {
  id: InjectionId
  status: 'applied' | 'not_triggered' | 'blocked'
  blocked?: PreviewBlockReason
  source?: string
}

export interface PromptInjectionPreviewResponse {
  endpoint: string
  items: PromptInjectionPreviewItem[]
  features: {
    thinkingModelName: boolean
    thinkingTrigger: ThinkingTriggerMode
    channelLeakGuard: boolean
    modelDrivenWebSearch: boolean
  }
  externalSystem?: string[]
  systemTurn?: string
  currentTurn?: string
  tools: Array<[string, string]>
  emptyTurnRetryTurn?: string
  error?: string
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
  externalPoolRetryStatusCodes: number[]
  externalPoolRetryOnNetworkError: boolean
  externalPoolRetryOnProtocolError: boolean
  externalPoolSamePoolRetryCount: number
  externalPoolSamePoolRetryStatusCodes: number[]
  externalPoolSamePoolRetryDelayMs: number
  externalPoolTransientFailurePriorityPenalty: number
  externalPoolTransientFailureCooldownThreshold: number
  externalPoolQualityAwareSchedulingEnabled: boolean
  externalPoolQualityEwmaAlpha: number
  externalPoolQualitySampleTtlSecs: number
  externalPoolQualityMinSamples: number
  externalPoolQualityPriorityWeight: number
  externalPoolQualityLoadWeight: number
  externalPoolQualityErrorWeight: number
  externalPoolQualityLatencyWeight: number
  externalPoolQualityProbationWeight: number
  externalPoolQualityTopK: number
  externalPoolDegradeWindowSecs: number
  externalPoolDegradeErrorRateThreshold: number
  externalPoolDegradeProbationSecs: number
  externalPoolMaxProbationSecs: number
  externalPoolProbeSharePercent: number
  externalPoolRecoveryRampSecs: number
  externalDirectPolicyEnabled: boolean
  directExternalOnLocalMaintenance: boolean
  directExternalModelRules: string[]
  directExternalPathRules: string[]
  externalPoolRouteMode: ExternalPoolRouteMode
  externalPoolRouteRules: string[]
  localPoolRouteMode: ExternalPoolRouteMode
  localPoolRouteRules: string[]
  fallbackOnLocalCapacityExhausted: boolean
  fallbackOnSchedulerRedisDegraded: boolean
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
  externalPoolModelUnavailableCooldownMode: ExternalPoolModelUnavailableCooldownMode
  externalPoolModelUnavailableCooldownSecs: number
  externalPoolRequestTimeoutSecs: number
  externalPoolStreamRequestTimeoutSecs: number
  externalPoolStreamIdleTimeoutSecs: number
  /** 首个语义输出的最长等待秒数（每个外部池尝试单独计时）；超时且未输出时换池，0 表示不限制。 */
  externalPoolStreamFirstOutputTimeoutSecs: number
  externalPoolStreamPreOutputRetryEnabled: boolean
  externalPoolAutoDisableOnChannelDisabled: boolean
  externalPoolUsageProjectionUpliftPercent: number
  externalPoolUsageProjectionCostFloorEnabled: boolean
  externalPoolUsageProjectionCostFloorMarginPercent: number
  externalPoolUsageProjectionOutputUpliftMinTokens: number
  externalPoolUsageProjectionOutputUpliftPercent: number
  externalPoolStreamResponseMode: ExternalPoolStreamResponseMode
  externalPoolUsageDebugEnabled: boolean
  externalPoolUsageDebugDir: string
  externalPoolUsageDebugMaxBodyBytes: number
  externalPoolUsageDebugMaxFiles: number
}

export interface ExternalPool {
  id: number
  name: string
  baseUrl: string
  apiKey?: string
  maskedApiKey?: string
  authType: ExternalPoolAuthType
  headerProfile: ExternalPoolHeaderProfile
  appendBetaQuery: boolean
  headerOverrides: Record<string, string>
  wireProfile: ExternalPoolWireProfile
  tlsProfile: ExternalPoolTlsProfile
  enabled: boolean
  priority: number
  maxConcurrentRequests: number
  usageProjectionMode: ExternalPoolUsageProjectionMode
  streamResponseMode?: ExternalPoolStreamResponseMode
  requestBodyMode: ExternalPoolRequestBodyMode
  rawModelMode: ExternalPoolRawModelMode
  autoDisablePolicy: ExternalPoolAutoDisablePolicy
  preOutputStreamRetryMode: ExternalPoolStreamRetryMode
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
  routeMode: ExternalPoolRouteMode
  routeRules: string[]
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
  transientFailureStreak: number
  transientFailureTtlSecs: number
  dispatchable: boolean
  skippedReason?: string
  quality?: ExternalPoolQualityView
}

export interface ExternalPoolQualityView {
  recentErrorRate: number
  ttftEwmaMs?: number
  latencyEwmaMs?: number
  sampleCount: number
  scoringActive: boolean
  inProbation: boolean
  probationRemainingSecs: number
  probationLevel: number
  recoveryProgress: number
}

export interface ExternalPoolsStatusResponse {
  pools: ExternalPoolStatus[]
}

export interface CreateExternalPoolRequest {
  name: string
  baseUrl: string
  apiKey: string
  authType?: ExternalPoolAuthType
  headerProfile?: ExternalPoolHeaderProfile
  appendBetaQuery?: boolean
  headerOverrides?: Record<string, string>
  wireProfile?: ExternalPoolWireProfile
  tlsProfile?: ExternalPoolTlsProfile
  enabled?: boolean
  priority?: number
  maxConcurrentRequests?: number
  usageProjectionMode?: ExternalPoolUsageProjectionMode
  streamResponseMode?: ExternalPoolStreamResponseMode | null
  requestBodyMode?: ExternalPoolRequestBodyMode
  rawModelMode?: ExternalPoolRawModelMode
  autoDisablePolicy?: ExternalPoolAutoDisablePolicy
  preOutputStreamRetryMode?: ExternalPoolStreamRetryMode
  preservePath?: boolean
  normalizeModelVersionDots?: boolean
  modelMappingMode?: ExternalPoolModelMappingMode
  modelMappingRequireMatch?: boolean
  modelMappingRules?: ExternalPoolModelMappingRule[]
  supportedModels?: string[]
  routeMode?: ExternalPoolRouteMode
  routeRules?: string[]
  notes?: string
}

export interface UpdateExternalPoolRequest {
  name?: string
  baseUrl?: string
  apiKey?: string
  authType?: ExternalPoolAuthType
  headerProfile?: ExternalPoolHeaderProfile
  appendBetaQuery?: boolean
  headerOverrides?: Record<string, string>
  wireProfile?: ExternalPoolWireProfile
  tlsProfile?: ExternalPoolTlsProfile
  enabled?: boolean
  priority?: number
  maxConcurrentRequests?: number
  usageProjectionMode?: ExternalPoolUsageProjectionMode
  streamResponseMode?: ExternalPoolStreamResponseMode | null
  requestBodyMode?: ExternalPoolRequestBodyMode
  rawModelMode?: ExternalPoolRawModelMode
  autoDisablePolicy?: ExternalPoolAutoDisablePolicy
  preOutputStreamRetryMode?: ExternalPoolStreamRetryMode
  preservePath?: boolean
  normalizeModelVersionDots?: boolean
  modelMappingMode?: ExternalPoolModelMappingMode
  modelMappingRequireMatch?: boolean
  modelMappingRules?: ExternalPoolModelMappingRule[]
  supportedModels?: string[]
  routeMode?: ExternalPoolRouteMode
  routeRules?: string[]
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

/** Targeted request capture for offline replay (files under `dir`, off by default). */
export interface RequestCaptureConfig {
  enabled: boolean
  dir: string
  usageOnly: boolean
  toolChoice: boolean
  maxFiles: number
  maxBodyBytes: number
}

export interface RequestAdmissionConfig {
  rpm: number
  maxConcurrentRequests: number
  maxQueuedRequests: number
  queueTimeoutMs: number
}

export interface AuxiliaryUpstreamRuntime {
  configuredLimit: number
  inFlight: number
  peakInFlight: number
  rejected: number
  refreshClientCacheEntries: number
  refreshClientCacheMaxEntries: number
  refreshClientBuilds: number
  refreshClientHits: number
  refreshClientMisses: number
  refreshClientCacheSaturated: number
}

export interface TokenRefreshAdmissionRuntime {
  authority: 'process_local' | 'redis_global' | 'redis_global_degraded'
  configuredRpm: number
  configuredBurst: number
  admitted: number
  rateLimited: number
  coordinationRejected: number
  redisErrors: number
  lastRetryAfterMs: number
  remainingMilliTokens: number
}

export interface RuntimeConfig {
  proxyUrl?: string | null
  proxyUsername?: string | null
  proxyPassword?: string | null
  credentialRpm: number
  requestAdmission: RequestAdmissionConfig
  credentialMaxConcurrentRequests: number
  credentialInfoRefreshConcurrency: number
  /** 每个 Kiro 积分的真实成本（美元），只用于总览把积分换算成成本；null 表示未配置 */
  kiroCreditUnitPriceUsd?: number | null
  credentialTransientCooldownSecs: number
  credentialRateLimitCooldownSecs: number
  credentialServerErrorCooldownSecs: number
  credentialNetworkErrorCooldownSecs: number
  credentialStreamErrorCooldownSecs: number
  /** 空回（只有用量统计、零帧或空响应体）是否让账号冷却，默认关闭。 */
  credentialEmptyTurnCooldownEnabled: boolean
  credentialUpstreamFailureCooldownEnabled: boolean
  credentialAutoHealMinIntervalSecs: number
  credentialAutoHealMaxConsecutiveRounds: number
  /** 自动同步 Kiro 客户端版本号（默认关闭）。 */
  kiroVersionAutoSyncEnabled: boolean
  /** 最近一次自动同步读到的官方版本。 */
  kiroVersionLatest?: string
  /** IDE 端点的对话和联网搜索改走 runtime.{region}.kiro.dev（默认关闭）。 */
  kiroRuntimeDomainEnabled: boolean
  credentialProtocolErrorCooldownSecs: number
  credentialAuthErrorCooldownSecs: number
  credentialCooldownBackoffMultiplier: number
  credentialCooldownJitterPercent: number
  credentialProbationSecs: number
  credentialMaxCooldownSecs: number
  credentialDispatchMaxWaitSecs: number
  kiroUpstreamResponseTimeoutSecs: number
  kiroUpstreamStreamIdleTimeoutSecs: number
  streamKeepaliveIntervalSecs: number
  streamPreOutputHoldSecs: number
  streamFirstOutputTimeoutSecs: number
  kiroUpstreamStreamRetryEnabled: boolean
  kiroUpstreamStreamRetryMaxAttempts: number
  inferenceUpstreamMaxAttempts: number
  auxiliaryUpstreamMaxAttempts: number
  auxiliaryUpstreamMaxConcurrentRequests: number
  auxiliaryUpstreamRuntime: AuxiliaryUpstreamRuntime
  tokenRefreshMaxRpm: number
  tokenRefreshBurst: number
  tokenRefreshBackgroundEnabled: boolean
  tokenRefreshBackgroundIntervalSecs: number
  tokenRefreshBackgroundLeadSecs: number
  tokenRefreshAdmissionRuntime: TokenRefreshAdmissionRuntime
  kiroUpstreamStreamRetryOnIdleTimeout: boolean
  kiroUpstreamStreamRetryOnReadError: boolean
  kiroUpstreamStreamRetryOnStatusError: boolean
  credentialRetryMaxAttempts: number
  credentialPromptLogicRetryEnabled: boolean
  credentialPromptLogicRetryMaxAttempts: number
  kiroUpstreamRegionRotationEnabled: boolean
  localBerserkModeEnabled: boolean
  localBerserkMaxRounds: number
  localBerserkRoundDelayMs: number
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
  promptInjection: PromptInjectionConfig
  routeFeatures: RouteFeaturesConfig
  claudeModelsOnly: boolean
  missingMaxTokens: MissingMaxTokensConfig
  requestCapture: RequestCaptureConfig
  payloadGuardEnabled: boolean
  payloadGuardMode: PayloadGuardMode
  payloadGuardMaxBytes: number
  payloadGuardKiroMaxWeight: number
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
  exposeProxyWarnings: boolean
  auditLogRetentionEnabled: boolean
  auditLogRetentionDays: number
  definedCacheRoutes: string[]
}

export type UpdateRuntimeConfigRequest = RuntimeConfig

export interface AccessKeysResponse {
  requestApiKey: string
  maskedRequestApiKey: string
  requestApiKeys: RequestApiKeyItem[]
  defaultRequestAdmission: RequestAdmissionConfig
  adminApiKey: string
  maskedAdminApiKey: string
}

export type KeyLimitMetric = 'cost_usd' | 'credits' | 'tokens'
export type KeyLimitPeriod = 'daily' | 'monthly' | 'total'

export interface KeyUsageLimit {
  metric: KeyLimitMetric
  period: KeyLimitPeriod
  limit: number
}

export interface RequestApiKeyItem {
  id: string
  apiKey: string
  maskedApiKey: string
  primary: boolean
  name: string
  enabled: boolean
  requestAdmission?: RequestAdmissionConfig
  usageLimits: KeyUsageLimit[]
}

export interface KeyLimitStatus {
  metric: KeyLimitMetric
  period: KeyLimitPeriod
  limit: number
  used: number
  exceeded: boolean
  /** RFC3339; absent for `total`. */
  resetsAt?: string | null
}

export interface RequestKeyUsageLimitItem {
  keyId: string
  enabled: boolean
  statuses: KeyLimitStatus[]
  /** Counters could not be read; requests with this key are rejected. */
  error?: string
}

export interface RequestKeyUsageLimitsResponse {
  timezone: string
  items: RequestKeyUsageLimitItem[]
}

export interface CreateRequestApiKeyRequest {
  apiKey?: string
  name?: string
  enabled?: boolean
  requestAdmission?: RequestAdmissionConfig
  usageLimits?: KeyUsageLimit[]
}

export interface UpdateRequestApiKeyRequest {
  apiKey?: string
  name?: string
  enabled?: boolean
  requestAdmission?: RequestAdmissionConfig
  usageLimits?: KeyUsageLimit[]
}

export interface UpdateAdminApiKeyRequest {
  adminApiKey: string
}

export type LoadBalancingMode = 'priority' | 'balanced' | 'health_balanced' | 'weighted_least_inflight'

export interface ModelPricing {
  inputCostPerToken: number
  outputCostPerToken: number
  /** 5 分钟缓存写入单价 */
  cacheCreationInputTokenCost: number
  /** 1 小时缓存写入单价；来源没有时按 input 的 2 倍 */
  cacheCreation1hInputTokenCost: number
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
  reasoningFields?: Record<string, {
    path: 'output_config' | 'reasoning'
    efforts: string[]
    defaultEffort?: string
  }>
}

export interface SyncModelCapabilitiesRequest {
  credentialIds?: number[]
}

export interface ManualModelPricingRequest {
  inputCostPerMillion: number
  outputCostPerMillion: number
  cacheCreationInputCostPerMillion?: number
  cacheCreation1hInputCostPerMillion?: number
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

// ============ 路由场景 ============

/** A route rule set, as used by every route policy. */
export interface RoutePolicy {
  routeMode: ExternalPoolRouteMode
  routeRules: string[]
}

export interface AccountRoutePolicy extends RoutePolicy {
  id: number
}

/** Scenario body. Accounts not listed are reset to "unrestricted" when applied. */
export interface RouteScenarioContent {
  local: RoutePolicy
  external: RoutePolicy
  localAccounts: AccountRoutePolicy[]
  externalAccounts: AccountRoutePolicy[]
}

export interface RouteScenario {
  id: number
  name: string
  description?: string | null
  content: RouteScenarioContent
  revision: number
  createdAt: string
  updatedAt: string
}

export interface RouteScenarioListResponse {
  scenarios: RouteScenario[]
  activeScenarioId?: number | null
  activeInSync: boolean
}

export interface RouteScenarioOptions {
  routes: string[]
  localAccounts: RouteScenarioAccountOption[]
  externalAccounts: RouteScenarioAccountOption[]
}

export interface SaveRouteScenarioRequest {
  name: string
  description?: string
  content: RouteScenarioContent
}

export type RouteScenarioItemKind = 'local_pool' | 'external_pool' | 'local_account' | 'external_account'

export interface RouteScenarioItemKey {
  kind: RouteScenarioItemKind
  id: number
}

export interface RouteScenarioChange extends RouteScenarioItemKey {
  label: string
  before: string
  after: string
}

export interface RouteScenarioPreview {
  changes: RouteScenarioChange[]
  unchanged: number
  missingAccounts: RouteScenarioItemKey[]
  unknownRoutes: string[]
  uncoveredLocalRoutes: string[]
  uncoveredExternalRoutes: string[]
}

export interface RouteScenarioApplyItem extends RouteScenarioItemKey {
  label: string
  success: boolean
  error?: string
}

export interface RouteScenarioApplyResponse {
  total: number
  success: number
  failed: number
  skipped: number
  items: RouteScenarioApplyItem[]
}

export interface RouteScenarioAccountOption extends RoutePolicy {
  id: number
  label: string
}

// ============================================================================
// Account inventory
// ============================================================================

/** Scheduling defaults applied server-side when accounts are imported into the pool. */
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
  proxyResourceIds?: number[]
  endpoint?: string
  warmupRemaining?: number
  enableOverageAfterImport?: boolean
  supportedModels?: string[]
  tags?: string[]
}

export type InventoryStatus = 'stored' | 'invalid' | 'pushing' | 'pushed'

export interface InventoryImportDefaults {
  authRegion?: string
  apiRegion?: string
  proxyResourceId?: number
  proxyResourceIds?: number[]
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
}

export interface InventoryImportRequest {
  credentials: AddCredentialRequest[]
  defaults?: InventoryImportDefaults
  check?: boolean
  source?: string
}

export interface InventoryImportItem {
  index: number
  ok: boolean
  skipped: boolean
  inventoryId?: number
  email?: string
  message?: string
}

export interface InventoryImportResponse {
  total: number
  imported: number
  skipped: number
  failed: number
  items: InventoryImportItem[]
}

export interface InventoryPoolLink {
  credentialId: number
  exists: boolean
  disabled: boolean
  disabledReason?: string
  subscriptionTitle?: string
}

export interface InventoryItem {
  id: number
  status: InventoryStatus
  authMethod: string
  email?: string
  provider?: string
  authRegion?: string
  apiRegion?: string
  machineId?: string
  hasProxy: boolean
  proxyResourceId?: number
  proxyResourceName?: string
  proxyUsable?: boolean
  proxyUrl?: string
  expiresAt?: string
  accountInfo?: BalanceResponse
  lastCheckAt?: string
  lastCheckError?: string
  lastRefreshAt?: string
  lastRefreshError?: string
  refreshFailureCount: number
  source?: string
  note?: string
  pool?: InventoryPoolLink
  pushedAt?: string
  createdAt: string
  updatedAt: string
}

export interface InventoryCounts {
  total: number
  stored: number
  invalid: number
  pushing: number
  pushed: number
}

export interface InventoryListResponse {
  page: number
  limit: number
  total: number
  totalPages: number
  counts: InventoryCounts
  items: InventoryItem[]
}

export interface InventoryListQuery {
  page?: number
  limit?: number
  status?: string
  q?: string
}

export interface InventoryUpdateRequest {
  authRegion?: string
  apiRegion?: string
  proxyResourceId?: number | null
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  note?: string
}

export interface InventoryPushRequest {
  ids: number[]
  defaults?: BatchCredentialImportDefaults
  autoDiscoverSupportedModels?: boolean
}

export interface InventoryActionItem {
  id: number
  ok: boolean
  credentialId?: number
  message?: string
}

export interface InventoryActionResponse {
  total: number
  succeeded: number
  failed: number
  items: InventoryActionItem[]
}
