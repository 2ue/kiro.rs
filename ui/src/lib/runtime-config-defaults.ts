import type {
  CacheHitShapingPolicyPatch,
  CachePolicyConfig,
  CacheRoutePolicyPatch,
  BodyConversionConfig,
  ImageProcessingConfig,
  MissingMaxTokensConfig,
  RequestCaptureConfig,
  ModelMappingConfig,
  PayloadShapingConfig,
  PromptCacheCreationControlConfig,
  PromptInjectionConfig,
  RouteFeaturesConfig,
  ReportedUsageConfig,
  ReportedUsageFieldMode,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RuntimeConfig,
  WeightedCapacityConfig,
} from '@/types/api'

const DEFAULT_OUTPUT_UPLIFT_MIN_TOKENS = 1000
const DEFAULT_OUTPUT_UPLIFT_PERCENT = 50
const DEFAULT_FINAL_CACHE_CREATION_MAX_TOKENS = 400000
const DEFAULT_FINAL_CACHE_CREATION_JITTER_MIN_TOKENS = 20000
const DEFAULT_FINAL_CACHE_CREATION_JITTER_MAX_TOKENS = 45000
const DEFAULT_FINAL_OUTPUT_MAX_TOKENS = 200000
const DEFAULT_FINAL_OUTPUT_JITTER_MIN_TOKENS = 5000
const DEFAULT_FINAL_OUTPUT_JITTER_MAX_TOKENS = 12000

export function preserveFieldPolicy(): ReportedUsageFieldPolicy {
  return {
    mode: 'preserve',
    maxTokens: 0,
    targetTokens: 0,
    normalMaxMultiplier: 1.1,
    moveDeltaToCacheRead: false,
  }
}

export function rawFieldPolicy(): ReportedUsageFieldPolicy {
  return { ...preserveFieldPolicy(), mode: 'raw' }
}

export function inputSamplePolicy(maxTokens = 96): ReportedUsageFieldPolicy {
  return { ...preserveFieldPolicy(), mode: 'sample-max', maxTokens, moveDeltaToCacheRead: true }
}

export function writerSamplePolicy(targetTokens = 3000, normalMaxMultiplier = 1.2): ReportedUsageFieldPolicy {
  return { ...preserveFieldPolicy(), mode: 'sample-target', targetTokens, normalMaxMultiplier }
}

export function pathPolicy(
  enabled = true,
  input: ReportedUsageFieldPolicy = rawFieldPolicy(),
  cacheCreation: ReportedUsageFieldPolicy = preserveFieldPolicy()
): ReportedUsagePathPolicy {
  return {
    enabled,
    skipNonStreamUsageProjection: false,
    finalCacheReadMaxTokens: 700000,
    finalCacheReadJitterMinTokens: 0,
    finalCacheReadJitterMaxTokens: 0,
    finalCacheCreationMaxTokens: DEFAULT_FINAL_CACHE_CREATION_MAX_TOKENS,
    finalCacheCreationJitterMinTokens: DEFAULT_FINAL_CACHE_CREATION_JITTER_MIN_TOKENS,
    finalCacheCreationJitterMaxTokens: DEFAULT_FINAL_CACHE_CREATION_JITTER_MAX_TOKENS,
    finalOutputGuardEnabled: true,
    outputUpliftMinTokens: DEFAULT_OUTPUT_UPLIFT_MIN_TOKENS,
    outputUpliftPercent: DEFAULT_OUTPUT_UPLIFT_PERCENT,
    finalOutputMaxTokens: DEFAULT_FINAL_OUTPUT_MAX_TOKENS,
    finalOutputJitterMinTokens: DEFAULT_FINAL_OUTPUT_JITTER_MIN_TOKENS,
    finalOutputJitterMaxTokens: DEFAULT_FINAL_OUTPUT_JITTER_MAX_TOKENS,
    input,
    output: rawFieldPolicy(),
    cacheRead: preserveFieldPolicy(),
    cacheCreation,
  }
}

export function defaultReportedUsage(): ReportedUsageConfig {
  return {
    default: pathPolicy(),
    pathOverrides: {
      '/cc': pathPolicy(true, inputSamplePolicy(96), writerSamplePolicy(3000)),
      '/ha': pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy()),
    },
  }
}

export function defaultCachePolicy(): CachePolicyConfig {
  return {
    default: {},
    currentHighCache: {},
    kiroRsTool: {},
    stableSegmentCache: {},
    kiroRsToolPlus: {},
    boundedHighCache: {},
    pathOverrides: {},
  }
}

export function defaultPayloadShaping(): PayloadShapingConfig {
  return {
    enabled: true,
    truncateHistoricalToolResults: true,
    historicalToolResultMaxChars: 8000,
    historicalToolResultHeadLines: 80,
    historicalToolResultTailLines: 40,
    discardHistoricalThinking: true,
    compressToolDefinitions: true,
    toolDefinitionsBudgetBytes: 20000,
    toolDescriptionMaxChars: 4000,
    toolSchemaAnnotationMaxChars: 1000,
    webFetchTrimEnabled: true,
    webFetchBodyMaxChars: 12000,
    fitCurrentPayloadToBudget: false,
    truncateCurrentToolResults: false,
    currentToolResultMaxChars: 80000,
    truncateCurrentUserContent: false,
    currentUserContentMaxChars: 120000,
    truncateCurrentDocuments: false,
    currentDocumentMaxChars: 80000,
    truncateCurrentImages: false,
    currentImagesMaxBytes: 180000,
    oversizedImageHandling: 'drop-with-placeholder',
  }
}

export function defaultImageProcessing(): ImageProcessingConfig {
  return {
    mode: 'safe' as const,
    safeMaterializeFileSources: true,
    safeDownloadRemoteSources: true,
    safeNormalizeBase64MediaTypes: true,
  }
}

export function defaultBodyConversion(): BodyConversionConfig {
  return {
    toolSchemaNormalization: true,
    toolNameMapping: true,
    toolSchemaKeyMapping: 'sanitize',
    toolSchemaKeyValidationRegex: '^[a-zA-Z0-9_.-]{1,64}$',
    toolChoiceSteering: true,
    nativeReasoningFields: true,
    syntheticThinkingSignatures: true,
    toolPairingRepair: true,
    historyPlaceholderTools: true,
    codeExecution: { enabled: false, pathOverrides: {}, timeoutMs: 5000, maxOutputBytes: 16384, maxCodeBytes: 65536 },
  }
}

export function normalizeBodyConversion(input?: Partial<BodyConversionConfig> | null): BodyConversionConfig {
  const base = defaultBodyConversion()
  return {
    toolSchemaNormalization: input?.toolSchemaNormalization ?? base.toolSchemaNormalization,
    toolNameMapping: input?.toolNameMapping ?? base.toolNameMapping,
    toolSchemaKeyMapping: input?.toolSchemaKeyMapping ?? base.toolSchemaKeyMapping,
    toolSchemaKeyValidationRegex: input?.toolSchemaKeyValidationRegex ?? base.toolSchemaKeyValidationRegex,
    toolChoiceSteering: input?.toolChoiceSteering ?? base.toolChoiceSteering,
    nativeReasoningFields: input?.nativeReasoningFields ?? base.nativeReasoningFields,
    syntheticThinkingSignatures: input?.syntheticThinkingSignatures ?? base.syntheticThinkingSignatures,
    toolPairingRepair: input?.toolPairingRepair ?? base.toolPairingRepair,
    historyPlaceholderTools: input?.historyPlaceholderTools ?? base.historyPlaceholderTools,
    codeExecution: {
      enabled: input?.codeExecution?.enabled ?? base.codeExecution.enabled,
      pathOverrides: { ...(input?.codeExecution?.pathOverrides ?? base.codeExecution.pathOverrides) },
      timeoutMs: input?.codeExecution?.timeoutMs ?? base.codeExecution.timeoutMs,
      maxOutputBytes: input?.codeExecution?.maxOutputBytes ?? base.codeExecution.maxOutputBytes,
      maxCodeBytes: input?.codeExecution?.maxCodeBytes ?? base.codeExecution.maxCodeBytes,
    },
  }
}

/**
 * Placeholder until the runtime config loads; the backend always returns the full structure.
 * Item defaults come from the catalog endpoint, so nothing here duplicates backend text.
 */
export function emptyPromptInjection(): PromptInjectionConfig {
  return {
    applyToCountTokens: true,
    defaults: {} as PromptInjectionConfig['defaults'],
    texts: {},
    pathOverrides: {},
  }
}

export function normalizePromptInjection(input?: Partial<PromptInjectionConfig> | null): PromptInjectionConfig {
  const base = emptyPromptInjection()
  const texts: PromptInjectionConfig['texts'] = {}
  for (const [key, value] of Object.entries(input?.texts ?? {})) {
    if (typeof value === 'string' && value.trim()) texts[key as keyof typeof texts] = value
  }
  const pathOverrides: PromptInjectionConfig['pathOverrides'] = {}
  for (const [prefix, items] of Object.entries(input?.pathOverrides ?? {})) {
    const kept = Object.fromEntries(
      Object.entries(items ?? {}).filter(([, item]) => item && (item.enabled !== undefined || item.externalPool !== undefined)),
    )
    if (Object.keys(kept).length > 0) pathOverrides[prefix] = kept
  }
  return {
    applyToCountTokens: input?.applyToCountTokens ?? base.applyToCountTokens,
    defaults: { ...base.defaults, ...(input?.defaults ?? {}) },
    texts,
    pathOverrides,
  }
}

export function defaultRouteFeatures(): RouteFeaturesConfig {
  return {
    defaults: {
      thinkingModelName: true,
      thinkingTrigger: 'real_request',
      channelLeakGuard: false,
      channelLeakGuardStream: true,
      modelDrivenWebSearch: false,
    },
    pathOverrides: {},
  }
}

export function normalizeRouteFeatures(input?: Partial<RouteFeaturesConfig> | null): RouteFeaturesConfig {
  const base = defaultRouteFeatures()
  const pathOverrides: RouteFeaturesConfig['pathOverrides'] = {}
  for (const [prefix, route] of Object.entries(input?.pathOverrides ?? {})) {
    const kept = Object.fromEntries(Object.entries(route ?? {}).filter(([, value]) => value !== undefined && value !== null))
    if (Object.keys(kept).length > 0) pathOverrides[prefix] = kept
  }
  return { defaults: { ...base.defaults, ...(input?.defaults ?? {}) }, pathOverrides }
}

export function defaultMissingMaxTokens(): MissingMaxTokensConfig {
  return {
    policy: 'default_value',
    defaultValue: 20480,
  }
}

export function normalizeMissingMaxTokens(input?: Partial<MissingMaxTokensConfig> | null): MissingMaxTokensConfig {
  const base = defaultMissingMaxTokens()
  const policy = input?.policy === 'reject' ? 'reject' : base.policy
  return {
    policy,
    defaultValue: toWhole(input?.defaultValue ?? base.defaultValue, 1, 200000),
  }
}

export function defaultRequestCapture(): RequestCaptureConfig {
  return {
    enabled: false,
    dir: 'logs/request-capture',
    usageOnly: true,
    toolChoice: true,
    maxFiles: 200,
    maxBodyBytes: 2 * 1024 * 1024,
  }
}

export function normalizeRequestCapture(input?: Partial<RequestCaptureConfig> | null): RequestCaptureConfig {
  const base = defaultRequestCapture()
  return {
    enabled: input?.enabled ?? base.enabled,
    dir: input?.dir?.trim() || base.dir,
    usageOnly: input?.usageOnly ?? base.usageOnly,
    toolChoice: input?.toolChoice ?? base.toolChoice,
    maxFiles: toWhole(input?.maxFiles ?? base.maxFiles, 1, 2000),
    maxBodyBytes: toWhole(input?.maxBodyBytes ?? base.maxBodyBytes, 1024, 8 * 1024 * 1024),
  }
}

export function defaultWeightedCapacity(): WeightedCapacityConfig {
  return {
    enabled: false,
    maxUnitsPerRequest: 8,
    tiers: [
      { minTokens: 0, units: 1 },
      { minTokens: 100000, units: 2 },
      { minTokens: 300000, units: 4 },
      { minTokens: 700000, units: 8 },
    ],
  }
}

export function normalizeWeightedCapacity(input?: Partial<WeightedCapacityConfig> | null): WeightedCapacityConfig {
  const base = defaultWeightedCapacity()
  const maxUnitsPerRequest = toWhole(input?.maxUnitsPerRequest ?? base.maxUnitsPerRequest, 1, 64)
  const tiers = (input?.tiers?.length ? input.tiers : base.tiers)
    .map((tier) => ({
      minTokens: toWhole(tier.minTokens),
      units: toWhole(tier.units, 1, maxUnitsPerRequest),
    }))
    .sort((a, b) => a.minTokens - b.minTokens)
    .filter((tier, index, all) => all.findIndex((item) => item.minTokens === tier.minTokens) === index)

  return {
    enabled: Boolean(input?.enabled ?? base.enabled),
    maxUnitsPerRequest,
    tiers: tiers.length ? tiers : base.tiers,
  }
}

export function normalizeImageProcessing(input?: Partial<ImageProcessingConfig> | null): ImageProcessingConfig {
  const next: ImageProcessingConfig = {
    ...defaultImageProcessing(),
    ...(input ?? {}),
  }
  if (next.mode === 'light') {
    return {
      mode: 'light',
      safeMaterializeFileSources: false,
      safeDownloadRemoteSources: false,
      safeNormalizeBase64MediaTypes: false,
    }
  }
  return {
    mode: 'safe',
    safeMaterializeFileSources: Boolean(next.safeMaterializeFileSources),
    safeDownloadRemoteSources: Boolean(next.safeDownloadRemoteSources),
    safeNormalizeBase64MediaTypes: Boolean(next.safeNormalizeBase64MediaTypes),
  }
}

export function defaultCacheHitShaping(): Required<CacheHitShapingPolicyPatch> {
  return {
    enabled: false,
    targetHitRatio: 0.9,
    ratioJitter: 0.04,
    minCacheReadTokens: 1024,
    minCacheCreationTokens: 1024,
    minCostRatio: 1,
    maxReadMultiplier: 0,
    minCreationRatio: 0,
    readAwareCostFloor: false,
  }
}

export function defaultPromptCacheCreationControl(): PromptCacheCreationControlConfig {
  return {
    enabled: true,
    scopeMode: 'conversation_model',
    minSuccessfulRequestsBetweenCreation: 3,
    minCreationIntervalSecs: 60,
    minCreationDeltaTokens: 12000,
    maxCreationTokensPerEvent: 30000,
    creationBudgetWindowSecs: 300,
    maxCreationTokensPerWindow: 120000,
    expireAfterIdleSecs: 3600,
  }
}

export function defaultExternalPoolsConfig() {
  return {
    externalPoolsEnabled: false,
    externalPoolGlobalMaxConcurrentRequests: 0,
    externalPoolMaxQueuedRequests: 0,
    externalPoolMaxInputTokens: 1000000,
    externalPoolCapacityMode: 'fail_fast' as const,
    externalPoolDispatchMaxWaitSecs: 30,
    externalPoolRetryMaxAttempts: 0,
    externalPoolRetryStatusCodes: [408, 425, 429, 500, 502, 503, 504, 529],
    externalPoolRetryOnNetworkError: true,
    externalPoolRetryOnProtocolError: true,
    externalPoolSamePoolRetryCount: 3,
    externalPoolSamePoolRetryStatusCodes: [401, 403, 429, 500, 502, 503, 504],
    externalPoolSamePoolRetryDelayMs: 500,
    externalPoolTransientFailurePriorityPenalty: 20,
    externalPoolTransientFailureCooldownThreshold: 0,
    externalPoolQualityAwareSchedulingEnabled: false,
    externalPoolQualityEwmaAlpha: 0.2,
    externalPoolQualitySampleTtlSecs: 600,
    externalPoolQualityMinSamples: 5,
    externalPoolQualityPriorityWeight: 1,
    externalPoolQualityLoadWeight: 100,
    externalPoolQualityErrorWeight: 100,
    externalPoolQualityLatencyWeight: 10,
    externalPoolQualityProbationWeight: 50,
    externalPoolQualityTopK: 3,
    externalPoolDegradeWindowSecs: 120,
    externalPoolDegradeErrorRateThreshold: 0.5,
    externalPoolDegradeProbationSecs: 180,
    externalPoolMaxProbationSecs: 900,
    externalPoolProbeSharePercent: 5,
    externalPoolRecoveryRampSecs: 60,
    externalDirectPolicyEnabled: false,
    directExternalOnLocalMaintenance: false,
    directExternalModelRules: [],
    directExternalPathRules: [],
    externalPoolRouteMode: 'allow_all' as const,
    externalPoolRouteRules: [],
    localPoolRouteMode: 'allow_all' as const,
    localPoolRouteRules: [],
    fallbackOnLocalCapacityExhausted: true,
    fallbackOnSchedulerRedisDegraded: true,
    fallbackOnNoAvailableCredentials: true,
    fallbackOnLocalTransientExhausted: true,
    fallbackOnUnsupportedModel: false,
    localPoolPreflightEnabled: true,
    externalPoolLocalRescueEnabled: true,
    externalPoolLocalRescueOnRateLimit: true,
    externalPoolLocalRescueOnTimeout: true,
    externalPoolLocalRescueOnCapacity: true,
    externalPoolLocalRescueMaxWaitSecs: 15,
    localPoolCircuitEnabled: true,
    localPoolCircuitWindowSecs: 60,
    localPoolCircuitOpenAfterFailures: 3,
    localPoolCircuitRequireDistinctCredentials: 2,
    localPoolCircuitOpenSecs: 30,
    externalPoolAutoDisableEnabled: false,
    externalPoolAutoDisableOnAuthError: true,
    externalPoolAutoDisableOnSecurityLock: true,
    externalPoolAutoDisableOnQuotaExhausted: false,
    externalPoolAutoDisableOnMisconfiguredEndpoint: false,
    externalPoolAutoDisableFailureThreshold: 1,
    externalPoolAutoDisableWindowSecs: 60,
    externalPoolAutoDisableDurationSecs: 0,
    externalPoolRateLimitCooldownSecs: 30,
    externalPoolServerErrorCooldownSecs: 10,
    externalPoolNetworkErrorCooldownSecs: 10,
    externalPoolProtocolErrorCooldownSecs: 10,
    externalPoolModelUnavailableCooldownMode: 'model' as const,
    externalPoolModelUnavailableCooldownSecs: 10,
    externalPoolRequestTimeoutSecs: 180,
    externalPoolStreamRequestTimeoutSecs: 0,
    externalPoolStreamIdleTimeoutSecs: 180,
    externalPoolStreamFirstOutputTimeoutSecs: 120,
    externalPoolStreamPreOutputRetryEnabled: true,
    externalPoolAutoDisableOnChannelDisabled: true,
    externalPoolUsageProjectionUpliftPercent: 25,
    externalPoolUsageProjectionCostFloorEnabled: true,
    externalPoolUsageProjectionCostFloorMarginPercent: 10,
    externalPoolUsageProjectionOutputUpliftMinTokens: 0,
    externalPoolUsageProjectionOutputUpliftPercent: 0,
    externalPoolStreamResponseMode: 'event_passthrough' as const,
    externalPoolUsageDebugEnabled: false,
    externalPoolUsageDebugDir: '/tmp/kiro-rs/external-pool-usage-debug',
    externalPoolUsageDebugMaxBodyBytes: 8192,
    externalPoolUsageDebugMaxFiles: 1000,
  }
}

export function defaultModelMappingConfig() {
  return {
    enabled: true,
    autoGenerateRules: true,
    allowVersionFallback: true,
    allowCrossFamilyFallback: false,
    rules: [],
  }
}

export function normalizeModelMapping(config?: Partial<ModelMappingConfig> | null): ModelMappingConfig {
  return {
    ...defaultModelMappingConfig(),
    ...(config ?? {}),
    rules: (config?.rules ?? [])
      .map((rule) => ({
        enabled: rule.enabled !== false,
        source: rule.source.trim().toLowerCase(),
        target: rule.target.trim().toLowerCase(),
        kind: rule.kind || 'alias',
        note: rule.note?.trim() || null,
      }))
      .filter((rule) => rule.source && rule.target),
  }
}

export const emptyRuntimeConfig: RuntimeConfig = {
  proxyUrl: null,
  proxyUsername: null,
  proxyPassword: null,
  credentialRpm: 0,
  requestAdmission: {
    rpm: 300,
    maxConcurrentRequests: 32,
    maxQueuedRequests: 64,
    queueTimeoutMs: 1000,
  },
  credentialMaxConcurrentRequests: 0,
  credentialInfoRefreshConcurrency: 3,
  kiroCreditUnitPriceUsd: null,
  credentialTransientCooldownSecs: 10,
  credentialRateLimitCooldownSecs: 30,
  credentialServerErrorCooldownSecs: 5,
  credentialNetworkErrorCooldownSecs: 5,
  credentialStreamErrorCooldownSecs: 5,
  credentialEmptyTurnCooldownEnabled: false,
  credentialProtocolErrorCooldownSecs: 10,
  credentialAuthErrorCooldownSecs: 10,
  credentialCooldownBackoffMultiplier: 2,
  credentialCooldownJitterPercent: 20,
  credentialProbationSecs: 30,
  credentialMaxCooldownSecs: 300,
  credentialDispatchMaxWaitSecs: 120,
  kiroUpstreamResponseTimeoutSecs: 180,
  kiroUpstreamStreamIdleTimeoutSecs: 180,
  streamKeepaliveIntervalSecs: 5,
  streamPreOutputHoldSecs: 15,
  streamFirstOutputTimeoutSecs: 0,
  kiroUpstreamStreamRetryEnabled: true,
  kiroUpstreamStreamRetryMaxAttempts: 6,
  inferenceUpstreamMaxAttempts: 4,
  auxiliaryUpstreamMaxAttempts: 2,
  auxiliaryUpstreamMaxConcurrentRequests: 16,
  tokenRefreshMaxRpm: 60,
  tokenRefreshBurst: 8,
  tokenRefreshBackgroundEnabled: true,
  tokenRefreshBackgroundIntervalSecs: 60,
  tokenRefreshBackgroundLeadSecs: 600,
  tokenRefreshAdmissionRuntime: {
    authority: 'process_local',
    configuredRpm: 60,
    configuredBurst: 8,
    admitted: 0,
    rateLimited: 0,
    coordinationRejected: 0,
    redisErrors: 0,
    lastRetryAfterMs: 0,
    remainingMilliTokens: 8000,
  },
  auxiliaryUpstreamRuntime: {
    configuredLimit: 16,
    inFlight: 0,
    peakInFlight: 0,
    rejected: 0,
    refreshClientCacheEntries: 0,
    refreshClientCacheMaxEntries: 256,
    refreshClientBuilds: 0,
    refreshClientHits: 0,
    refreshClientMisses: 0,
    refreshClientCacheSaturated: 0,
  },
  kiroUpstreamStreamRetryOnIdleTimeout: true,
  kiroUpstreamStreamRetryOnReadError: true,
  kiroUpstreamStreamRetryOnStatusError: true,
  credentialRetryMaxAttempts: 0,
  credentialPromptLogicRetryEnabled: false,
  credentialPromptLogicRetryMaxAttempts: 0,
  kiroUpstreamRegionRotationEnabled: false,
  localBerserkModeEnabled: false,
  localBerserkMaxRounds: 1,
  localBerserkRoundDelayMs: 1000,
  credentialInFlightLeaseMaxSecs: 900,
  dispatchGlobalMaxConcurrentRequests: 0,
  dispatchMaxQueuedRequests: 0,
  weightedCapacity: defaultWeightedCapacity(),
  credentialWarmupRequests: 3,
  credentialWarmupSelectionPercent: 5,
  credentialWarmupMaxSelectionPercent: 50,
  schedulerErrorEwmaAlpha: 0.2,
  schedulerPriorityWeight: 1,
  schedulerLoadWeight: 100,
  schedulerErrorWeight: 100,
  schedulerLatencyWeight: 0.01,
  schedulerProbationWeight: 50,
  schedulerSelectionPressureWeight: 25,
  schedulerTotalSelectionWeight: 0,
  schedulerTopK: 3,
  selectionFailureSampleLimit: 20,
  selectionFailureRecordEnabled: true,
  compressionEnabled: false,
  whitespaceCompression: true,
  imageProcessing: defaultImageProcessing(),
  bodyConversion: defaultBodyConversion(),
  promptInjection: emptyPromptInjection(),
  routeFeatures: defaultRouteFeatures(),
  claudeModelsOnly: false,
  missingMaxTokens: defaultMissingMaxTokens(),
  requestCapture: defaultRequestCapture(),
  payloadGuardEnabled: true,
  payloadGuardMode: 'preemptive',
  payloadGuardMaxBytes: 460800,
  payloadGuardKiroMaxWeight: 1300000,
  payloadGuardSafetyMarginBytes: 32768,
  payloadGuardTrimHistory: true,
  payloadGuardExternalEnabled: true,
  kiroCachePointEnabled: false,
  kiroCachePointToolsOnly: true,
  kiroCachePointRecordPlan: true,
  payloadShaping: defaultPayloadShaping(),
  promptCacheTargetReadRatio: 0.98,
  promptCacheTokenScale: 1.6,
  promptCacheMaxSimulatedInputTokens: 300000,
  promptCacheCapJitterMinTokens: 12000,
  promptCacheCapJitterMaxTokens: 24000,
  promptCacheScaleMinInputTokens: 20000,
  promptCacheCreationControl: defaultPromptCacheCreationControl(),
  promptCacheMaxEntriesPerAccount: 200,
  promptCacheMaxEntriesGlobal: 20000,
  promptCacheEntryTtlSecs: 86400,
  promptCacheEstimatedBytesLimit: 268435456,
  reportedUsage: defaultReportedUsage(),
  cachePolicy: defaultCachePolicy(),
  externalPools: defaultExternalPoolsConfig(),
  highCacheThreshold: 10000,
  compatProfile: 'claude-code',
  kiroAgentModeStrategy: 'vibe',
  modelResolutionMode: 'compatible',
  modelMapping: defaultModelMappingConfig(),
  extractThinking: true,
  exposeProxyWarnings: false,
  auditLogRetentionEnabled: false,
  auditLogRetentionDays: 15,
  definedCacheRoutes: [],
}

export function reportedUsageModeDescription(mode: ReportedUsageFieldMode): string {
  switch (mode) {
    case 'raw':
      return '显示服务收到或返回的原始用量，不再额外调整缓存、输入或输出数值。'
    case 'preserve':
      return '保留系统已经计算好的展示结果，适合大多数默认场景。'
    case 'sample-max':
      return '把展示值控制在上限以内，并让数值自然浮动。需要配置“展示上限”。'
    case 'sample-target':
      return '让展示值围绕目标值自然浮动，并受最大倍率限制。'
  }
}

export function fieldNeedsMax(policy: ReportedUsageFieldPolicy): boolean {
  return policy.mode === 'sample-max'
}

export function fieldNeedsTarget(policy: ReportedUsageFieldPolicy): boolean {
  return policy.mode === 'sample-target'
}

export function toWhole(value: number, min = 0, max?: number): number {
  const normalized = Math.max(min, Math.floor(value || 0))
  return typeof max === 'number' ? Math.min(max, normalized) : normalized
}

/// 夹紧浮点配置值。与 toRatio 不同，上界可自定义且允许取到 1.0——
/// 质量评分权重可以远大于 1，失败率阈值也允许设成 1.0 表示"全失败才降级"。
export function clampFloat(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min
  return Math.min(max, Math.max(min, Number(value.toFixed(4))))
}

export function toRatio(value: number): number {
  if (!Number.isFinite(value)) return 0
  return Math.min(0.99, Math.max(0, Number(value.toFixed(4))))
}

export function toScale(value: number): number {
  if (!Number.isFinite(value)) return 1
  return Math.min(3, Math.max(1, Number(value.toFixed(2))))
}

function toMultiplier(value: number): number {
  if (!Number.isFinite(value)) return 1.1
  return Math.max(1, Number(value.toFixed(2)))
}

function normalizeFieldPolicy(policy: ReportedUsageFieldPolicy): ReportedUsageFieldPolicy {
  return {
    ...policy,
    maxTokens: toWhole(policy.maxTokens),
    targetTokens: toWhole(policy.targetTokens),
    normalMaxMultiplier: toMultiplier(policy.normalMaxMultiplier),
  }
}

function normalizePathPolicy(policy: ReportedUsagePathPolicy): ReportedUsagePathPolicy {
  const finalCacheReadMaxTokens = toWhole(policy.finalCacheReadMaxTokens ?? 700000)
  const finalCacheReadJitterMaxTokens =
    finalCacheReadMaxTokens > 0
      ? toWhole(policy.finalCacheReadJitterMaxTokens ?? 0, 0, finalCacheReadMaxTokens)
      : 0
  const finalCacheReadJitterMinTokens = toWhole(
    policy.finalCacheReadJitterMinTokens ?? 0,
    0,
    finalCacheReadJitterMaxTokens
  )
  const finalCacheCreationMaxTokens = toWhole(
    policy.finalCacheCreationMaxTokens ?? DEFAULT_FINAL_CACHE_CREATION_MAX_TOKENS
  )
  const finalCacheCreationJitterMaxTokens =
    finalCacheCreationMaxTokens > 0
      ? toWhole(
          policy.finalCacheCreationJitterMaxTokens ?? DEFAULT_FINAL_CACHE_CREATION_JITTER_MAX_TOKENS,
          0,
          finalCacheCreationMaxTokens
        )
      : 0
  const finalCacheCreationJitterMinTokens = toWhole(
    policy.finalCacheCreationJitterMinTokens ?? DEFAULT_FINAL_CACHE_CREATION_JITTER_MIN_TOKENS,
    0,
    finalCacheCreationJitterMaxTokens
  )
  const finalOutputMaxTokens = toWhole(policy.finalOutputMaxTokens ?? DEFAULT_FINAL_OUTPUT_MAX_TOKENS)
  const finalOutputJitterMaxTokens =
    finalOutputMaxTokens > 0
      ? toWhole(
          policy.finalOutputJitterMaxTokens ?? DEFAULT_FINAL_OUTPUT_JITTER_MAX_TOKENS,
          0,
          finalOutputMaxTokens
        )
      : 0
  const finalOutputJitterMinTokens = toWhole(
    policy.finalOutputJitterMinTokens ?? DEFAULT_FINAL_OUTPUT_JITTER_MIN_TOKENS,
    0,
    finalOutputJitterMaxTokens
  )
  return {
    ...policy,
    skipNonStreamUsageProjection: Boolean(policy.skipNonStreamUsageProjection),
    finalCacheReadMaxTokens,
    finalCacheReadJitterMinTokens,
    finalCacheReadJitterMaxTokens,
    finalCacheCreationMaxTokens,
    finalCacheCreationJitterMinTokens,
    finalCacheCreationJitterMaxTokens,
    finalOutputGuardEnabled: policy.finalOutputGuardEnabled ?? true,
    outputUpliftMinTokens: toWhole(policy.outputUpliftMinTokens ?? DEFAULT_OUTPUT_UPLIFT_MIN_TOKENS),
    outputUpliftPercent: toWhole(policy.outputUpliftPercent ?? DEFAULT_OUTPUT_UPLIFT_PERCENT, 0, 200),
    finalOutputMaxTokens,
    finalOutputJitterMinTokens,
    finalOutputJitterMaxTokens,
    input: normalizeFieldPolicy(policy.input),
    output: normalizeFieldPolicy(policy.output),
    cacheRead: normalizeFieldPolicy(policy.cacheRead),
    cacheCreation: normalizeFieldPolicy(policy.cacheCreation),
  }
}

export function normalizeReportedUsage(config: ReportedUsageConfig): ReportedUsageConfig {
  const pathOverrides = Object.fromEntries(
    Object.entries(config.pathOverrides)
      .map(([prefix, policy]) => {
        const trimmed = prefix.trim()
        if (!trimmed) return null
        const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
        const normalizedPrefix = withSlash.replace(/\/+$/, '') || '/'
        return [normalizedPrefix, normalizePathPolicy(policy)] as const
      })
      .filter((entry): entry is readonly [string, ReportedUsagePathPolicy] => Boolean(entry))
  )
  return {
    default: normalizePathPolicy(config.default),
    pathOverrides,
  }
}

function normalizeCachePolicyPathPrefix(prefix: string): string | null {
  const trimmed = prefix.trim()
  if (!trimmed) return null
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  return canonicalCachePolicyPath(withSlash.replace(/\/+$/, '') || '/')
}

function canonicalCachePolicyPath(prefix: string): string {
  const normalized = prefix.replace(/\/+$/, '') || '/'
  const lower = normalized.toLowerCase()
  if (lower === '/cc/v1' || lower === '/cc/v1/messages') return '/cc'
  if (lower === '/ha/v1' || lower === '/ha/v1/messages') return '/ha'
  if (lower === '/na/v1' || lower === '/na/v1/messages') return '/na'
  return normalized
}

function isEmptyCachePolicyPatch(policy: CacheRoutePolicyPatch): boolean {
  return !policy.cacheType && policy.routeNamespace === undefined && !policy.simulation && !policy.creationControl && !policy.reportedUsage && !policy.cachePoint && !policy.bounds && !policy.kiroRsTool && !policy.kiroRsToolPlus && !policy.stableSegment && !policy.hitShaping && !policy.coldRequestGuard
}

export function normalizeCachePolicy(config?: CachePolicyConfig): CachePolicyConfig {
  const source = config ?? defaultCachePolicy()
  const pathOverrides = Object.fromEntries(
    Object.entries(source.pathOverrides ?? {})
      .map(([prefix, policy]) => {
        const normalizedPrefix = normalizeCachePolicyPathPrefix(prefix)
        if (!normalizedPrefix || isEmptyCachePolicyPatch(policy)) return null
        return [normalizedPrefix, policy] as const
      })
      .filter((entry): entry is readonly [string, CacheRoutePolicyPatch] => Boolean(entry))
  )
  return {
    default: source.default ?? {},
    currentHighCache: source.currentHighCache ?? {},
    kiroRsTool: source.kiroRsTool ?? {},
    stableSegmentCache: source.stableSegmentCache ?? {},
    kiroRsToolPlus: source.kiroRsToolPlus ?? {},
    boundedHighCache: source.boundedHighCache ?? {},
    pathOverrides,
  }
}

export function normalizePromptCacheCreationControl(
  config: PromptCacheCreationControlConfig
): PromptCacheCreationControlConfig {
  return {
    ...defaultPromptCacheCreationControl(),
    ...config,
    scopeMode:
      config.scopeMode === 'credential_conversation_model'
        ? 'credential_conversation_model'
        : 'conversation_model',
    minSuccessfulRequestsBetweenCreation: toWhole(config.minSuccessfulRequestsBetweenCreation),
    minCreationIntervalSecs: toWhole(config.minCreationIntervalSecs),
    minCreationDeltaTokens: toWhole(config.minCreationDeltaTokens),
    maxCreationTokensPerEvent: toWhole(config.maxCreationTokensPerEvent),
    creationBudgetWindowSecs: toWhole(config.creationBudgetWindowSecs),
    maxCreationTokensPerWindow: toWhole(config.maxCreationTokensPerWindow),
    expireAfterIdleSecs: toWhole(config.expireAfterIdleSecs),
  }
}

export function normalizePayloadShaping(config?: Partial<PayloadShapingConfig> | null): PayloadShapingConfig {
  const next = {
    ...defaultPayloadShaping(),
    ...(config ?? {}),
  }
  return {
    ...next,
    historicalToolResultMaxChars: toWhole(next.historicalToolResultMaxChars),
    historicalToolResultHeadLines: toWhole(next.historicalToolResultHeadLines),
    historicalToolResultTailLines: toWhole(next.historicalToolResultTailLines),
    toolDefinitionsBudgetBytes: toWhole(next.toolDefinitionsBudgetBytes),
    toolDescriptionMaxChars: toWhole(next.toolDescriptionMaxChars),
    toolSchemaAnnotationMaxChars: toWhole(next.toolSchemaAnnotationMaxChars),
    webFetchBodyMaxChars: toWhole(next.webFetchBodyMaxChars),
    currentToolResultMaxChars: toWhole(next.currentToolResultMaxChars),
    currentUserContentMaxChars: toWhole(next.currentUserContentMaxChars),
    currentDocumentMaxChars: toWhole(next.currentDocumentMaxChars),
    currentImagesMaxBytes: toWhole(next.currentImagesMaxBytes),
    oversizedImageHandling: next.oversizedImageHandling ?? 'drop-with-placeholder',
  }
}
