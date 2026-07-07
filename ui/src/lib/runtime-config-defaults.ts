import type {
  CachePolicyConfig,
  CacheRoutePolicyPatch,
  BodyConversionConfig,
  ImageProcessingConfig,
  PayloadShapingConfig,
  PromptCacheCreationControlConfig,
  ReportedUsageConfig,
  ReportedUsageFieldMode,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RuntimeConfig,
  WeightedCapacityConfig,
} from '@/types/api'

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
    toolChoiceSteering: true,
    chunkedToolPolicy: true,
    thinkingPromptControls: true,
    nativeReasoningFields: true,
    toolPairingRepair: true,
    historyPlaceholderTools: true,
  }
}

export function normalizeBodyConversion(input?: Partial<BodyConversionConfig> | null): BodyConversionConfig {
  return {
    ...defaultBodyConversion(),
    ...(input ?? {}),
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
    externalPoolCapacityMode: 'fail_fast' as const,
    externalPoolDispatchMaxWaitSecs: 30,
    externalPoolRetryMaxAttempts: 0,
    externalDirectPolicyEnabled: false,
    directExternalOnLocalMaintenance: false,
    directExternalModelRules: [],
    directExternalPathRules: [],
    fallbackOnLocalCapacityExhausted: true,
    fallbackOnNoAvailableCredentials: true,
    fallbackOnLocalTransientExhausted: true,
    fallbackOnUnsupportedModel: false,
    localPoolPreflightEnabled: true,
    externalPoolLocalRescueEnabled: true,
    externalPoolLocalRescueOnRateLimit: true,
    externalPoolLocalRescueOnTimeout: true,
    externalPoolLocalRescueOnCapacity: true,
    externalPoolLocalRescueMaxWaitSecs: 15,
    localPoolCircuitEnabled: false,
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
    externalPoolRequestTimeoutSecs: 180,
    externalPoolStreamRequestTimeoutSecs: 0,
    externalPoolStreamIdleTimeoutSecs: 180,
    externalPoolAutoDisableOnChannelDisabled: true,
    externalPoolUsageProjectionUpliftPercent: 25,
    externalPoolUsageProjectionOutputUpliftMinTokens: 0,
    externalPoolUsageProjectionOutputUpliftPercent: 0,
    externalPoolStreamResponseMode: 'event_passthrough_usage_rewrite' as const,
  }
}

export function defaultModelMappingConfig() {
  return {
    enabled: true,
    autoGenerateRules: true,
    rules: [],
  }
}

export const emptyRuntimeConfig: RuntimeConfig = {
  proxyUrl: null,
  proxyUsername: null,
  proxyPassword: null,
  credentialRpm: 0,
  credentialMaxConcurrentRequests: 0,
  credentialTransientCooldownSecs: 10,
  credentialRateLimitCooldownSecs: 30,
  credentialServerErrorCooldownSecs: 5,
  credentialNetworkErrorCooldownSecs: 5,
  credentialStreamErrorCooldownSecs: 5,
  credentialProtocolErrorCooldownSecs: 10,
  credentialAuthErrorCooldownSecs: 10,
  credentialCooldownBackoffMultiplier: 2,
  credentialCooldownJitterPercent: 20,
  credentialProbationSecs: 30,
  credentialMaxCooldownSecs: 300,
  credentialDispatchMaxWaitSecs: 120,
  kiroUpstreamResponseTimeoutSecs: 180,
  kiroUpstreamStreamIdleTimeoutSecs: 180,
  credentialRetryMaxAttempts: 0,
  credentialPromptLogicRetryEnabled: false,
  credentialPromptLogicRetryMaxAttempts: 0,
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
  payloadGuardEnabled: true,
  payloadGuardMode: 'preemptive',
  payloadGuardMaxBytes: 460800,
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
  thinkingTriggerMode: 'real_request',
  exposeProxyWarnings: false,
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
  return {
    ...policy,
    skipNonStreamUsageProjection: Boolean(policy.skipNonStreamUsageProjection),
    finalCacheReadMaxTokens,
    finalCacheReadJitterMinTokens,
    finalCacheReadJitterMaxTokens,
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
  return withSlash.replace(/\/+$/, '') || '/'
}

function isEmptyCachePolicyPatch(policy: CacheRoutePolicyPatch): boolean {
  return !policy.cacheType && !policy.simulation && !policy.creationControl && !policy.reportedUsage && !policy.cachePoint && !policy.bounds && !policy.kiroRsTool
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

export function normalizePayloadShaping(config: PayloadShapingConfig): PayloadShapingConfig {
  return {
    ...config,
    historicalToolResultMaxChars: toWhole(config.historicalToolResultMaxChars),
    historicalToolResultHeadLines: toWhole(config.historicalToolResultHeadLines),
    historicalToolResultTailLines: toWhole(config.historicalToolResultTailLines),
    toolDefinitionsBudgetBytes: toWhole(config.toolDefinitionsBudgetBytes),
    toolDescriptionMaxChars: toWhole(config.toolDescriptionMaxChars),
    toolSchemaAnnotationMaxChars: toWhole(config.toolSchemaAnnotationMaxChars),
    webFetchBodyMaxChars: toWhole(config.webFetchBodyMaxChars),
    currentToolResultMaxChars: toWhole(config.currentToolResultMaxChars),
    currentUserContentMaxChars: toWhole(config.currentUserContentMaxChars),
    currentDocumentMaxChars: toWhole(config.currentDocumentMaxChars),
    currentImagesMaxBytes: toWhole(config.currentImagesMaxBytes),
    oversizedImageHandling: config.oversizedImageHandling ?? 'drop-with-placeholder',
  }
}
