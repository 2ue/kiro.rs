import type {
  PayloadShapingConfig,
  PromptCacheCreationControlConfig,
  ReportedUsageConfig,
  ReportedUsageFieldMode,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RuntimeConfig,
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
      '/na': pathPolicy(false),
      '/cc': pathPolicy(true, inputSamplePolicy(96), writerSamplePolicy(3000)),
      '/ha': pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy()),
    },
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
  }
}

export function defaultPromptCacheCreationControl(): PromptCacheCreationControlConfig {
  return {
    enabled: false,
    scopeMode: 'credential_conversation_model',
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
    localPoolCircuitEnabled: false,
    localPoolCircuitWindowSecs: 60,
    localPoolCircuitOpenAfterFailures: 3,
    localPoolCircuitRequireDistinctCredentials: 2,
    localPoolCircuitOpenSecs: 30,
    localPoolCircuitHalfOpenMaxProbes: 1,
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
  credentialRetryMaxAttempts: 0,
  credentialInFlightLeaseMaxSecs: 900,
  dispatchGlobalMaxConcurrentRequests: 0,
  dispatchMaxQueuedRequests: 0,
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
  compressionEnabled: false,
  whitespaceCompression: true,
  payloadGuardEnabled: true,
  payloadGuardMode: 'preemptive',
  payloadGuardMaxBytes: 460800,
  payloadGuardTrimHistory: true,
  payloadShaping: defaultPayloadShaping(),
  promptCacheTargetReadRatio: 0.98,
  promptCacheTokenScale: 1.6,
  promptCacheMaxSimulatedInputTokens: 300000,
  promptCacheCapJitterMinTokens: 12000,
  promptCacheCapJitterMaxTokens: 24000,
  promptCacheScaleMinInputTokens: 20000,
  promptCacheCreationControl: defaultPromptCacheCreationControl(),
  reportedUsage: defaultReportedUsage(),
  externalPools: defaultExternalPoolsConfig(),
  highCacheThreshold: 10000,
  compatProfile: 'claude-code',
  modelResolutionMode: 'compatible',
  modelMapping: defaultModelMappingConfig(),
  extractThinking: true,
  exposeProxyWarnings: false,
}

export function reportedUsageModeDescription(mode: ReportedUsageFieldMode): string {
  switch (mode) {
    case 'raw':
      return '原始值表示这个字段不经过本地 high-cache 模拟、放大或路径采样改写。input 使用请求原始 token，output 优先使用上游返回值，缺失时使用本地输出估算。'
    case 'preserve':
      return '保留计算值表示这个字段使用 high-cache、上游 metadata 或本地估算完成后的缓存计算结果。它不是原始请求值。'
    case 'sample-max':
      return '按上限采样改写会把这个字段改写到上限以内，分布偏向较小值，不会固定贴着上限。需要配置“采样上限”。'
    case 'sample-target':
      return '按目标采样改写会围绕目标值生成自然浮动结果，常规最大值由“目标 tokens × 常规最大倍率”决定，并且不会超过当前可用字段值。'
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
  return {
    ...policy,
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

export function normalizePromptCacheCreationControl(
  config: PromptCacheCreationControlConfig
): PromptCacheCreationControlConfig {
  return {
    ...defaultPromptCacheCreationControl(),
    ...config,
    scopeMode:
      config.scopeMode === 'conversation_model'
        ? 'conversation_model'
        : 'credential_conversation_model',
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
  }
}
