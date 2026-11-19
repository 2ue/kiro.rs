import type { AccountsConfig } from "./api";
export type CompatProfile = "claude-code" | "anthropic-strict" | "debug";

export type ModelResolutionMode = "compatible" | "alias_only" | "exact_only";

export type ThinkingTriggerMode = "real_request" | "always" | "off";

export type ModelMappingRuleKind = "version_equivalent" | "alias" | "fallback";

export type PayloadGuardMode = "preemptive" | "on_too_long";

export type ModelMappingMode =
  | "passthrough"
  | "passthrough_mapping"
  | "direct_mapping"
  | "processed_mapping";

export type ReportedUsageFieldMode =
  | "raw"
  | "preserve"
  | "sample-max"
  | "sample-target";

export interface ModelMappingRule {
  enabled: boolean;
  source: string;
  target: string;
  kind: ModelMappingRuleKind;
  note?: string | null;
}

export interface ModelMappingConfig {
  enabled: boolean;
  autoGenerateRules: boolean;
  allowVersionFallback: boolean;
  allowCrossFamilyFallback: boolean;
  rules: ModelMappingRule[];
}

export interface ReportedUsageFieldPolicy {
  mode: ReportedUsageFieldMode;
  maxTokens: number;
  targetTokens: number;
  normalMaxMultiplier: number;
  moveDeltaToCacheRead: boolean;
}

export interface ReportedUsagePathPolicy {
  enabled: boolean;
  skipNonStreamUsageProjection: boolean;
  /** Streaming requests must meet the official cache conditions before shaping; off by default. */
  strictCacheEligibilityStream?: boolean;
  /** Non-streaming requests must meet the official cache conditions before shaping; off by default. */
  strictCacheEligibilityNonStream?: boolean;
  finalCacheReadMaxTokens: number;
  finalCacheReadJitterMinTokens: number;
  finalCacheReadJitterMaxTokens: number;
  finalCacheCreationMaxTokens: number;
  finalCacheCreationJitterMinTokens: number;
  finalCacheCreationJitterMaxTokens: number;
  finalOutputGuardEnabled: boolean;
  outputUpliftMinTokens: number;
  outputUpliftPercent: number;
  finalOutputMaxTokens: number;
  finalOutputJitterMinTokens: number;
  finalOutputJitterMaxTokens: number;
  input: ReportedUsageFieldPolicy;
  output: ReportedUsageFieldPolicy;
  cacheRead: ReportedUsageFieldPolicy;
  cacheCreation: ReportedUsageFieldPolicy;
}

export interface ReportedUsageConfig {
  default: ReportedUsagePathPolicy;
}

export interface CacheSimulationPolicyPatch {
  enabled?: boolean;
  targetReadRatio?: number;
  tokenScale?: number;
  maxSimulatedInputTokens?: number;
  capJitterMinTokens?: number;
  capJitterMaxTokens?: number;
  scaleMinInputTokens?: number;
}

export interface CachePointPolicyPatch {
  enabled?: boolean;
  toolsOnly?: boolean;
  recordPlan?: boolean;
}

export interface CacheBoundsPolicyPatch {
  maxEntriesPerAccount?: number;
  maxEntriesGlobal?: number;
  entryTtlSecs?: number;
  estimatedBytesLimit?: number;
}

export interface UsageFieldShaping {
  multiplier?: number;
  jitterRatio?: number;
  minTokens?: number;
  maxTokens?: number;
  maxJitterTokens?: number;
}

export interface UsageShapingProfile {
  input?: UsageFieldShaping;
  output?: UsageFieldShaping;
  cacheRead?: UsageFieldShaping;
  cacheCreation?: UsageFieldShaping;
  capOverflowToInput?: boolean;
  minCostRatio?: number;
}

export type UsageShapingProfileMode = "inherit" | "custom" | "raw";

export interface PrefixToolPlusCachePolicy {
  coverageRatio?: number;
  maxCoverageTokens?: number;
  incrementalCreateEnabled?: boolean;
  cacheCurrentUserStablePrefix?: boolean;
  currentUserStablePrefixMaxTokens?: number;
  creationFrequency?: PromptCacheCreationControlConfig;
  suppressedCreationTarget?: "input" | "cache_read" | "drop";
  shaping?: UsageShapingProfile;
  nonStream?: { mode?: UsageShapingProfileMode; profile?: UsageShapingProfile };
  smallRequest?: {
    enabled?: boolean;
    maxInputTokens?: number;
    disableCache?: boolean;
    mode?: UsageShapingProfileMode;
    profile?: UsageShapingProfile;
  };
}

export interface PrefixToolCachePolicyPatch {
  coverageRatio?: number;
  maxCoverageTokens?: number;
  incrementalCreateEnabled?: boolean;
  maxNewCreationTokensPerRequest?: number;
  cacheCurrentUserStablePrefix?: boolean;
  currentUserStablePrefixMaxTokens?: number;
}

export interface StableSegmentCachePolicyPatch {
  coverageRatio?: number;
  maxCoverageTokens?: number;
  minTotalInputTokens?: number;
  minSegmentTokens?: number;
  incrementalCreateEnabled?: boolean;
  maxNewCreationTokensPerRequest?: number;
  includeTools?: boolean;
  includeSystem?: boolean;
  includeHistory?: boolean;
  historyMessageLimit?: number;
  honorExplicitCacheControl?: boolean;
  autoCacheSystem?: boolean;
  autoCacheTools?: boolean;
  autoCacheHistoryMessageEnds?: boolean;
  cacheCurrentUserStablePrefix?: boolean;
  currentUserStablePrefixMaxTokens?: number;
  defaultTtlSecs?: number;
  extendedTtlSecs?: number;
  reportedInputMinTokens?: number;
  reportedInputMaxTokens?: number;
  tokenScale?: number;
  maxSimulatedInputTokens?: number;
  capJitterMinTokens?: number;
  capJitterMaxTokens?: number;
  scaleMinInputTokens?: number;
  outputUpliftPercent?: number;
  outputUpliftMinTokens?: number;
  finalCacheReadMaxTokens?: number;
  finalCacheReadJitterMinTokens?: number;
  finalCacheReadJitterMaxTokens?: number;
  finalCacheCreationMaxTokens?: number;
  finalCacheCreationJitterMinTokens?: number;
  finalCacheCreationJitterMaxTokens?: number;
  finalOutputMaxTokens?: number;
  finalOutputJitterMinTokens?: number;
  finalOutputJitterMaxTokens?: number;
}

export type PromptCacheStrategyType =
  | "no_cache"
  | "current_high_cache"
  | "prefix_tool"
  | "stable_segment_cache"
  | "prefix_tool_plus"
  | "bounded_high_cache";

export interface CacheRoutePolicyPatch {
  cacheType?: PromptCacheStrategyType;
  routeNamespace?: boolean;
  simulation?: CacheSimulationPolicyPatch;
  creationControl?: PromptCacheCreationControlConfig;
  reportedUsage?: ReportedUsagePathPolicy;
  cachePoint?: CachePointPolicyPatch;
  bounds?: CacheBoundsPolicyPatch;
  prefixTool?: PrefixToolCachePolicyPatch;
  prefixToolPlus?: PrefixToolPlusCachePolicy;
  stableSegment?: StableSegmentCachePolicyPatch;
  hitShaping?: CacheHitShapingPolicyPatch;
  coldRequestGuard?: ColdRequestGuardPolicyPatch;
  cacheTtl?: CacheTtlPolicyPatch;
}

export type CacheTtlMode =
  | "follow_request_default_5m"
  | "follow_request_default_1h"
  | "force_5m"
  | "force_1h"
  | "upstream_then_request_default_5m";

export interface CacheTtlPolicyPatch {
  account?: CacheTtlMode;
}

export interface ColdRequestGuardPolicyPatch {
  enabled?: boolean;
}

export interface CacheHitShapingPolicyPatch {
  enabled?: boolean;
  targetHitRatio?: number;
  ratioJitter?: number;
  minCacheReadTokens?: number;
  minCacheCreationTokens?: number;
  minCostRatio?: number;
  maxReadMultiplier?: number;
  minCreationRatio?: number;
  /** Count cache reads toward the cost floor; off keeps the original shaping. */
  readAwareCostFloor?: boolean;
  /** Top up the remaining cost gap with input tokens; off drops the gap instead. */
  costFloorTopUpInput?: boolean;
}

export interface CachePolicyConfig {
  default: CacheRoutePolicyPatch;
  currentHighCache: CacheRoutePolicyPatch;
  prefixTool: CacheRoutePolicyPatch;
  stableSegmentCache: CacheRoutePolicyPatch;
  prefixToolPlus?: CacheRoutePolicyPatch;
  /** 上下限优先缓存策略模板，不继承 default。 */
  boundedHighCache?: CacheRoutePolicyPatch;
}

export interface PromptCacheCreationControlConfig {
  enabled: boolean;
  scopeMode: "credential_conversation_model" | "conversation_model";
  minSuccessfulRequestsBetweenCreation: number;
  minCreationIntervalSecs: number;
  minCreationDeltaTokens: number;
  maxCreationTokensPerEvent: number;
  creationBudgetWindowSecs: number;
  maxCreationTokensPerWindow: number;
  expireAfterIdleSecs: number;
}

export type OversizedImageHandling = "drop-with-placeholder" | "reject";

export type ImageProcessingMode = "safe" | "light";

export interface ImageProcessingConfig {
  mode: ImageProcessingMode;
  safeMaterializeFileSources: boolean;
  safeDownloadRemoteSources: boolean;
  safeNormalizeBase64MediaTypes: boolean;
  /** 发往上游前压缩过大的图片（默认关闭）。 */
  resizeEnabled: boolean;
  resizeMaxLongSide: number;
  resizeMaxBytes: number;
  resizeJpegQuality: number;
}

export type InjectionId =
  | "taskQuality"
  | "languageConstraint"
  | "custom"
  | "identityPrompt";

export type InjectionTextKey =
  | "taskQuality"
  | "languageConstraint"
  | "languageTurnReminder.explicit"
  | "languageTurnReminder.followUser"
  | "custom"
  | "identityPrompt"
  | "identityTurnReminder"
  | "chunkedWritePolicy"
  | "chunkedWriteToolSuffix.write"
  | "chunkedWriteToolSuffix.edit"
  | "webSearchGuidance"
  | "emptyTurnNudge";

/** 一项的全局默认；后端 `ItemSetting` 只有 `enabled`，且拒绝未知字段。 */
export interface InjectionItemSetting {
  enabled: boolean;
}

/** 一项在某条路由上的设置；没写表示跟随全局默认。后端拒绝未知字段。 */
export interface InjectionItemOverride {
  enabled?: boolean;
}

export interface PromptInjectionConfig {
  applyToCountTokens: boolean;
  defaults: Record<InjectionId, InjectionItemSetting>;
  /** Edited texts only; a missing key uses the built-in text from the catalog. */
  texts: Partial<Record<InjectionTextKey, string>>;
}

export interface RouteFeatureSet {
  missingMaxTokens: MissingMaxTokensConfig | null;
  reasoningCompat: LocalRequestCompatConfig | null;
  historySanitize: boolean;
  thinkingModelName: boolean;
  thinkingTrigger: ThinkingTriggerMode;
  payloadGuard: RoutePayloadGuardConfig | null;
  payloadShaping: PayloadShapingConfig | null;
  imageProcessing: ImageProcessingConfig | null;
}

export type RouteFeatureOverride = Partial<RouteFeatureSet>;

export interface RouteFeaturesConfig {
  defaults: RouteFeatureSet;
}

export type InjectionCategory =
  | "operator"
  | "protocol"
  | "feature"
  | "recovery";

export type InjectionRecommendation = "keep_on" | "user_choice";

export interface InjectionTextVariant {
  key: InjectionTextKey;
  label: string;
  defaultText: string;
}

export interface InjectionCatalogEntry {
  id: InjectionId;
  code: string;
  name: string;
  category: InjectionCategory;
  position: string;
  trigger: string;
  dependsOn: string | null;
  impactWhenOff: string;
  recommendation: InjectionRecommendation;
  recommendationNote: string;
  defaultEnabled: boolean;
  strictBlocks: boolean;
  editable: boolean;
  texts: InjectionTextVariant[];
  readOnlyVariants: InjectionTextVariant[];
}

export interface FixedInjection {
  code: string;
  name: string;
  text: string;
  reason: string;
}

export interface PromptInjectionCatalog {
  items: InjectionCatalogEntry[];
  fixed: FixedInjection[];
  maxTextChars: number;
  maxTotalTextChars: number;
}

export type PreviewToolChoice = "auto" | "any" | "tool" | "none";

export type PreviewThinking = "off" | "enabled" | "adaptive";

export interface PromptInjectionPreviewRequest {
  route: string;
  model?: string;
  nativeReasoning?: boolean;
  withSystem?: boolean;
  withWriteEdit?: boolean;
  disableParallelToolUse?: boolean;
  toolChoice?: PreviewToolChoice;
  thinking?: PreviewThinking;
  language?: "none" | "declared";
  emptyTurnRetry?: boolean;
  stream?: boolean;
  /** Unsaved settings used only for this preview. */
  draft?: {
    promptInjection?: PromptInjectionConfig;
    routeFeatures?: RouteFeaturesConfig;
  };
}

export type PreviewBlockReason =
  | "off"
  | "account_unsupported"
  | "account_off"
  | "strict";

export interface PromptInjectionPreviewItem {
  id: InjectionId;
  status: "applied" | "not_triggered" | "blocked";
  blocked?: PreviewBlockReason;
  source?: string;
}

export interface PromptInjectionPreviewResponse {
  endpoint: string;
  items: PromptInjectionPreviewItem[];
  features: {
    thinkingModelName: boolean;
    thinkingTrigger: ThinkingTriggerMode;
    channelLeakGuard: boolean;
    modelDrivenWebSearch: boolean;
  };
  externalSystem?: string[];
  systemTurn?: string;
  currentTurn?: string;
  tools: Array<[string, string]>;
  emptyTurnRetryTurn?: string;
  error?: string;
}

export interface PayloadShapingConfig {
  enabled: boolean;
  truncateHistoricalToolResults: boolean;
  historicalToolResultMaxChars: number;
  historicalToolResultHeadLines: number;
  historicalToolResultTailLines: number;
  discardHistoricalThinking: boolean;
  compressToolDefinitions: boolean;
  toolDefinitionsBudgetBytes: number;
  toolDescriptionMaxChars: number;
  toolSchemaAnnotationMaxChars: number;
  webFetchTrimEnabled: boolean;
  webFetchBodyMaxChars: number;
  fitCurrentPayloadToBudget: boolean;
  truncateCurrentToolResults: boolean;
  currentToolResultMaxChars: number;
  truncateCurrentUserContent: boolean;
  currentUserContentMaxChars: number;
  truncateCurrentDocuments: boolean;
  currentDocumentMaxChars: number;
  truncateCurrentImages: boolean;
  currentImagesMaxBytes: number;
  oversizedImageHandling: OversizedImageHandling;
}

export interface WeightedCapacityTier {
  minTokens: number;
  units: number;
}

export interface WeightedCapacityConfig {
  enabled: boolean;
  maxUnitsPerRequest: number;
  tiers: WeightedCapacityTier[];
}

export type MissingMaxTokensPolicy = "reject" | "default_value";

export interface MissingMaxTokensConfig {
  policy: MissingMaxTokensPolicy;
  defaultValue: number;
}

export interface LocalRequestCompatConfig {
  dropEffortNone: boolean;
  thinkingTypeAliases: boolean;
  repairThinkingBudget: boolean;
  normalizeMessageRoles: boolean;
  normalizeMessageContent: boolean;
  normalizeEffortCase: boolean;
  dropUnknownEffort: boolean;
}

export interface RequestCaptureConfig {
  enabled: boolean;
  dir: string;
  usageOnly: boolean;
  toolChoice: boolean;
  maxFiles: number;
  maxBodyBytes: number;
}

export interface RequestAdmissionConfig {
  rpm: number;
  maxConcurrentRequests: number;
  maxQueuedRequests: number;
  queueTimeoutMs: number;
}

export interface AuxiliaryUpstreamRuntime {
  configuredLimit: number;
  inFlight: number;
  peakInFlight: number;
  rejected: number;
  refreshClientCacheEntries: number;
  refreshClientCacheMaxEntries: number;
  refreshClientBuilds: number;
  refreshClientHits: number;
  refreshClientMisses: number;
  refreshClientCacheSaturated: number;
}

export interface TokenRefreshAdmissionRuntime {
  authority: "process_local" | "redis_global" | "redis_global_degraded";
  configuredRpm: number;
  configuredBurst: number;
  admitted: number;
  rateLimited: number;
  coordinationRejected: number;
  redisErrors: number;
  lastRetryAfterMs: number;
  remainingMilliTokens: number;
}

export interface RuntimeConfig {
  proxyUrl?: string | null;
  proxyUsername?: string | null;
  /** 读取时是脱敏值（如 `ab...yz`），只表示“已设置”；写入时传明文即替换，不传则保持不变。 */
  proxyPassword?: string | null;
  tlsBackend: "rustls" | "native-tls";
  requestAdmission: RequestAdmissionConfig;
  streamPreOutputHoldSecs: number;
  inferenceUpstreamMaxAttempts: number;
  promptInjection: PromptInjectionConfig;
  routeFeatures: RouteFeaturesConfig;
  promptCacheTargetReadRatio: number;
  promptCacheTokenScale: number;
  promptCacheMaxSimulatedInputTokens: number;
  promptCacheCapJitterMinTokens: number;
  promptCacheCapJitterMaxTokens: number;
  promptCacheScaleMinInputTokens: number;
  promptCacheCreationControl: PromptCacheCreationControlConfig;
  promptCacheMaxEntriesPerAccount: number;
  promptCacheMaxEntriesGlobal: number;
  promptCacheEntryTtlSecs: number;
  promptCacheEstimatedBytesLimit: number;
  reportedUsage: ReportedUsageConfig;
  cachePolicy: CachePolicyConfig;
  accounts: AccountsConfig;
  highCacheThreshold: number;
  compatProfile: CompatProfile;
  modelResolutionMode: ModelResolutionMode;
  modelMapping: ModelMappingConfig;
  auditLogRetentionEnabled: boolean;
  auditLogRetentionDays: number;
  usageRecordRetentionEnabled: boolean;
  usageRecordRetentionDays: number;
  usageRecordLimit: number;
}

export type UpdateRuntimeConfigRequest = Partial<RuntimeConfig>;

export interface RoutePayloadGuardConfig {
  enabled: boolean;
  mode: PayloadGuardMode;
  maxBytes: number;
  safetyMarginBytes: number;
  trimHistory: boolean;
  externalEnabled: boolean;
  tooLongHandling: "client_compaction" | "trim_retry";
}
