export type CompatProfile = 'claude-code' | 'anthropic-strict' | 'debug'
export type KiroAgentModeStrategy = 'vibe' | 'spec' | 'auto'
export type ModelResolutionMode = 'compatible' | 'alias_only' | 'exact_only'
export type ThinkingTriggerMode = 'real_request' | 'always'
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
  routeNamespace?: boolean
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

export type PromptSteeringScope = 'route_rules' | 'cc_only' | 'claude_code_profile' | 'all_routes'
export type PromptSteeringRouteMode = 'allow_all' | 'allow_list' | 'deny_list'

export interface PromptSteeringTextBlock {
  enabled: boolean
  prompt: string
}

export interface PromptSteeringToggle {
  enabled: boolean
}

export interface ChunkedWritePromptSteeringConfig {
  enabled: boolean
  systemPromptEnabled: boolean
  toolDescriptionEnabled: boolean
}

export interface PromptSteeringConfig {
  enabled: boolean
  scope: PromptSteeringScope
  routeMode: PromptSteeringRouteMode
  routeRules: string[]
  applyToExternalPool: boolean
  applyToCountTokens: boolean
  languageConstraint: PromptSteeringTextBlock
  taskQuality: PromptSteeringTextBlock
  toolChoice: PromptSteeringToggle
  chunkedWrite: ChunkedWritePromptSteeringConfig
  thinking: PromptSteeringToggle
  custom: PromptSteeringTextBlock
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
  promptSteering: PromptSteeringConfig
  missingMaxTokens: MissingMaxTokensConfig
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
  thinkingTriggerMode: ThinkingTriggerMode
  exposeProxyWarnings: boolean
  definedCacheRoutes: string[]
}

export type UpdateRuntimeConfigRequest = RuntimeConfig
