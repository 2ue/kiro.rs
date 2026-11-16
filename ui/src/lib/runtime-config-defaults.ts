import type {
CacheHitShapingPolicyPatch,
CachePolicyConfig,
ImageProcessingConfig,
LocalRequestCompatConfig,
MissingMaxTokensConfig,
ModelMappingConfig,
PayloadShapingConfig,
PromptCacheCreationControlConfig,
PromptInjectionConfig,
ReportedUsageConfig,
ReportedUsageFieldMode,
ReportedUsageFieldPolicy,
ReportedUsagePathPolicy,
RequestCaptureConfig,
RouteFeaturesConfig,
RuntimeConfig,
WeightedCapacityConfig
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
    strictCacheEligibilityStream: false,
    strictCacheEligibilityNonStream: false,
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

export function defaultReportedUsage(): ReportedUsageConfig { return { default: pathPolicy(false) } }

export function defaultCachePolicy(): CachePolicyConfig {
  return {
    default: {},
    currentHighCache: {},
    prefixTool: {},
    stableSegmentCache: {},
    prefixToolPlus: {},
    boundedHighCache: {},
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
    resizeEnabled: false,
    resizeMaxLongSide: 1568,
    resizeMaxBytes: 400000,
    resizeJpegQuality: 85,
  }
}

export function emptyPromptInjection(): PromptInjectionConfig {
  return {
    applyToCountTokens: true,
    defaults: {} as PromptInjectionConfig['defaults'],
    texts: {},
  }
}

export function normalizePromptInjection(input?: Partial<PromptInjectionConfig> | null): PromptInjectionConfig {
  const base = emptyPromptInjection()
  const texts: PromptInjectionConfig['texts'] = {}
  for (const [key, value] of Object.entries(input?.texts ?? {})) {
    if (typeof value === 'string' && value.trim()) texts[key as keyof typeof texts] = value
  }
  return {
    applyToCountTokens: input?.applyToCountTokens ?? base.applyToCountTokens,
    defaults: { ...base.defaults, ...(input?.defaults ?? {}) },
    texts,
  }
}

export function defaultRouteFeatures(): RouteFeaturesConfig {
return { defaults: { thinkingModelName: false, thinkingTrigger: 'off', historySanitize: false, missingMaxTokens: null, reasoningCompat: null, payloadGuard: null, payloadShaping: null, imageProcessing: null } }
}
export function normalizeRouteFeatures(input?: Partial<RouteFeaturesConfig> | null): RouteFeaturesConfig {
return { defaults: { ...defaultRouteFeatures().defaults, ...input?.defaults } }
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

export function defaultLocalRequestCompat(): LocalRequestCompatConfig {
  return {
    dropEffortNone: true,
    thinkingTypeAliases: true,
    repairThinkingBudget: true,
    normalizeMessageRoles: true,
    normalizeMessageContent: true,
    normalizeEffortCase: true,
    dropUnknownEffort: true,
  }
}

/** Missing switches fall back to the default, so older servers keep today's behavior. */
export function normalizeLocalRequestCompat(input?: Partial<LocalRequestCompatConfig> | null): LocalRequestCompatConfig {
  const base = defaultLocalRequestCompat()
  const out = { ...base }
  for (const key of Object.keys(base) as (keyof LocalRequestCompatConfig)[]) {
    const value = input?.[key]
    if (typeof value === 'boolean') out[key] = value
  }
  return out
}

export function defaultRequestCapture(): RequestCaptureConfig {
  return {
    enabled: true,
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
      resizeEnabled: Boolean(next.resizeEnabled),
      resizeMaxLongSide: Math.min(8000, Math.max(256, Math.round(Number(next.resizeMaxLongSide) || 1568))),
      resizeMaxBytes: Math.min(5242880, Math.max(32768, Math.round(Number(next.resizeMaxBytes) || 400000))),
      resizeJpegQuality: Math.min(100, Math.max(40, Math.round(Number(next.resizeJpegQuality) || 85))),
    }
  }
  return {
    mode: 'safe',
    safeMaterializeFileSources: Boolean(next.safeMaterializeFileSources),
    safeDownloadRemoteSources: Boolean(next.safeDownloadRemoteSources),
    safeNormalizeBase64MediaTypes: Boolean(next.safeNormalizeBase64MediaTypes),
    resizeEnabled: Boolean(next.resizeEnabled),
    resizeMaxLongSide: Math.min(8000, Math.max(256, Math.round(Number(next.resizeMaxLongSide) || 1568))),
    resizeMaxBytes: Math.min(5242880, Math.max(32768, Math.round(Number(next.resizeMaxBytes) || 400000))),
    resizeJpegQuality: Math.min(100, Math.max(40, Math.round(Number(next.resizeJpegQuality) || 85))),
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
    costFloorTopUpInput: true,
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

export function defaultAccountsConfig(): import('@/types/api').AccountsConfig {
  return {
 wireDebugEnabled: false, wireDebugDir: "diagnostics/wire", wireDebugMaxBodyBytes: 1048576, wireDebugMaxFiles: 50,
    enabled: false,
    globalMaxConcurrentRequests: 0,
    maxQueuedRequests: 0,
    capacityMode: 'fail_fast' as const,
    dispatchMaxWaitSecs: 30,
    retryMaxAttempts: 0,
    retryStatusCodes: [408, 425, 429, 500, 502, 503, 504, 529],
    retryOnNetworkError: true,
    retryOnProtocolError: true,
    samePoolRetryCount: 3,
    samePoolRetryStatusCodes: [401, 403, 429, 500, 502, 503, 504],
    samePoolRetryDelayMs: 500,
    transientFailurePriorityPenalty: 20,
    qualityAwareSchedulingEnabled: false,
    qualityEwmaAlpha: 0.2,
    qualitySampleTtlSecs: 600,
    qualityMinSamples: 5,
    qualityPriorityWeight: 1,
    qualityLoadWeight: 100,
    qualityErrorWeight: 100,
    qualityLatencyWeight: 10,
    qualityProbationWeight: 50,
    qualityTopK: 3,
    degradeWindowSecs: 120,
    degradeErrorRateThreshold: 0.5,
    degradeProbationSecs: 180,
    maxProbationSecs: 900,
    probeSharePercent: 5,
    recoveryRampSecs: 60,
    autoDisableEnabled: false,
    autoDisableOnAuthError: true,
    autoDisableOnSecurityLock: true,
    autoDisableOnQuotaExhausted: false,
    autoDisableOnMisconfiguredEndpoint: false,
    autoDisableFailureThreshold: 1,
    autoDisableWindowSecs: 60,
    autoDisableDurationSecs: 0,
    rateLimitCooldownSecs: 30,
    serverErrorCooldownSecs: 10,
    networkErrorCooldownSecs: 10,
    protocolErrorCooldownSecs: 10,
    modelUnavailableCooldownMode: 'model' as const,
    modelUnavailableCooldownSecs: 10,
    requestTimeoutSecs: 180,
    streamRequestTimeoutSecs: 0,
    streamIdleTimeoutSecs: 180,
    streamFirstOutputTimeoutSecs: 120,
    streamPreOutputRetryEnabled: true,
    autoDisableOnChannelDisabled: true,
    usageProjectionUpliftPercent: 25,
    usageProjectionCostFloorEnabled: true,
    usageProjectionCostFloorMarginPercent: 10,
    usageProjectionOutputUpliftMinTokens: 0,
    usageProjectionOutputUpliftPercent: 0,
    streamResponseMode: 'event_passthrough' as const,
    usageDebugEnabled: false,
    usageDebugDir: '/tmp/claude-pool/account-usage-debug',
    usageDebugMaxBodyBytes: 8192,
    usageDebugMaxFiles: 1000,
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
 tlsBackend: 'rustls',
 usageRecordLimit: 5000,
  proxyUrl: null,
  proxyUsername: null,
  proxyPassword: null,
  requestAdmission: {
    rpm: 300,
    maxConcurrentRequests: 32,
    maxQueuedRequests: 64,
    queueTimeoutMs: 1000,
  },
  streamPreOutputHoldSecs: 15,
  inferenceUpstreamMaxAttempts: 4,
  promptInjection: emptyPromptInjection(),
  routeFeatures: defaultRouteFeatures(),
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
  accounts: defaultAccountsConfig(),
  highCacheThreshold: 10000,
  compatProfile: 'claude-code',
  modelResolutionMode: 'compatible',
  modelMapping: defaultModelMappingConfig(),
  auditLogRetentionEnabled: false,
  auditLogRetentionDays: 15,
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

export function normalizeReportedUsage(config: ReportedUsageConfig): ReportedUsageConfig { return { default: { ...pathPolicy(false), ...config.default } } }
export function normalizeCachePolicy(config?: CachePolicyConfig): CachePolicyConfig { return { ...defaultCachePolicy(), ...config } }

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
