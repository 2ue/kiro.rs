export type ReportedUsageFieldMode = 'raw' | 'preserve' | 'sample-max' | 'sample-target'

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
