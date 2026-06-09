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

export interface CredentialsPageQuery {
  page: number
  limit: number
  q?: string
  status?: string
  authMethod?: string
  subscription?: string
  proxyResourceId?: number
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
  inProbation?: boolean
  probationRemainingSecs?: number
  schedulerSelectionCount?: number
  recentSchedulerSelectionCount10s?: number
  recentSchedulerSelectionCount60s?: number
  recentSchedulerSelectionCount5m?: number
  schedulerSelectionPressure?: number
  schedulerScore?: number
  estimatedCostUsd: number
  pricedRequests: number
  unpricedRequests: number
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
  nextResetAt: number | null
}

export type CredentialInfoResponse = BalanceResponse

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

export interface AddCredentialRequest {
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'api_key'
  clientId?: string
  clientSecret?: string
  email?: string
  priority?: number
  maxConcurrentRequests?: number | null
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  proxyResourceId?: number | null
  kiroApiKey?: string
  endpoint?: string
}

export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
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
  upstreamModel?: string
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
  pricingAvailable: boolean
  pricingModel?: string
  durationMs: number
  firstTokenLatencyMs?: number
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
  payloadBreakdown?: unknown
  payloadGuardReport?: unknown
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
}

export interface ExternalPoolAttempt {
  attempt: number
  poolId: number
  poolName: string
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

export interface UsageDashboardResponse {
  generatedAt: string
  timezone: string
  windows: UsageDashboardWindow[]
  series: UsageDashboardSeries
  top: UsageDashboardTop
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
  pricedRequests: number
  unpricedRequests: number
  averageDurationMs: number
  p95DurationMs: number
  stickyBoundRequests: number
  fallbackFromStickyRequests: number
  simulatedRequests: number
  upstreamMetadataRequests: number
  externalPoolBilling?: UsageExternalPoolBillingSummary
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
}

export interface UsageRecordsQuery {
  limit?: number
  q?: string
  conversationId?: string
  credentialId?: number
  externalPoolId?: number
  model?: string
  status?: UsageRecordStatus
  source?: UsageSource
  stream?: boolean
  minCacheRead?: number
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
export type ModelResolutionMode = 'compatible' | 'alias_only' | 'exact_only'
export type ModelMappingRuleKind = 'version_equivalent' | 'alias' | 'fallback'
export type PayloadGuardMode = 'preemptive' | 'on_too_long'
export type ExternalPoolAuthType = 'bearer' | 'x_api_key'
export type ExternalPoolUsageProjectionMode = 'pass_through' | 'current_path_policy'
export type ExternalPoolAutoDisablePolicy = 'inherit' | 'disabled' | 'enabled'

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
  input: ReportedUsageFieldPolicy
  output: ReportedUsageFieldPolicy
  cacheRead: ReportedUsageFieldPolicy
  cacheCreation: ReportedUsageFieldPolicy
}

export interface ReportedUsageConfig {
  default: ReportedUsagePathPolicy
  pathOverrides: Record<string, ReportedUsagePathPolicy>
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
}

export interface ExternalPoolsConfig {
  externalPoolsEnabled: boolean
  externalPoolGlobalMaxConcurrentRequests: number
  externalPoolMaxQueuedRequests: number
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
  externalPoolLocalRescueMaxWaitSecs: number
  localPoolCircuitEnabled: boolean
  localPoolCircuitWindowSecs: number
  localPoolCircuitOpenAfterFailures: number
  localPoolCircuitRequireDistinctCredentials: number
  localPoolCircuitOpenSecs: number
  localPoolCircuitHalfOpenMaxProbes: number
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
  autoDisablePolicy: ExternalPoolAutoDisablePolicy
  autoDisabled: boolean
  autoDisabledReason?: string
  autoDisabledAt?: string
  autoDisabledUntil?: string
  autoDisabledLastError?: string
  preservePath: boolean
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
  autoDisablePolicy?: ExternalPoolAutoDisablePolicy
  preservePath?: boolean
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
  autoDisablePolicy?: ExternalPoolAutoDisablePolicy
  preservePath?: boolean
  notes?: string
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
  credentialRetryMaxAttempts: number
  credentialInFlightLeaseMaxSecs: number
  dispatchGlobalMaxConcurrentRequests: number
  dispatchMaxQueuedRequests: number
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
  compressionEnabled: boolean
  whitespaceCompression: boolean
  payloadGuardEnabled: boolean
  payloadGuardMode: PayloadGuardMode
  payloadGuardMaxBytes: number
  payloadGuardSafetyMarginBytes: number
  payloadGuardTrimHistory: boolean
  payloadShaping: PayloadShapingConfig
  promptCacheTargetReadRatio: number
  promptCacheTokenScale: number
  promptCacheMaxSimulatedInputTokens: number
  promptCacheCapJitterMinTokens: number
  promptCacheCapJitterMaxTokens: number
  promptCacheScaleMinInputTokens: number
  promptCacheCreationControl: PromptCacheCreationControlConfig
  reportedUsage: ReportedUsageConfig
  externalPools: ExternalPoolsConfig
  highCacheThreshold: number
  compatProfile: CompatProfile
  modelResolutionMode: ModelResolutionMode
  modelMapping: ModelMappingConfig
  extractThinking: boolean
  exposeProxyWarnings: boolean
}

export type UpdateRuntimeConfigRequest = RuntimeConfig

export interface AccessKeysResponse {
  requestApiKey: string
  maskedRequestApiKey: string
  adminApiKey: string
  maskedAdminApiKey: string
}

export interface UpdateAdminApiKeyRequest {
  adminApiKey: string
}

export type LoadBalancingMode = 'priority' | 'balanced' | 'health_balanced'

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
