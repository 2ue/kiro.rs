import type {
  ModelMappingMode,
  RawUpstreamError,
  RoutePolicy,
  RouteRuleMode,
} from "./api";
export interface AccountUsageSnapshot {
  totalInputTokens: number;
  inputTokens: number;
  billableInputTokens: number;
  outputTokens: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
  cacheCreation5mInputTokens: number;
  cacheCreation1hInputTokens: number;
}

export interface AccountBilling {
  requestInputTokens?: number;
  rawUsage: AccountUsageSnapshot;
  shapedUsage?: AccountUsageSnapshot;
  reportedUsage: AccountUsageSnapshot;
  usageProjectionApplied: boolean;
  rawCostUsd: number;
  shapedCostUsd?: number;
  upliftedCostUsd?: number;
  profitUsd?: number;
  reportedCostUsd: number;
  billableCostUsd: number;
  costFloorDeltaUsd: number;
  costFloorApplied: boolean;
  pricingAvailable: boolean;
  pricingModel?: string;
  usageProjectionMode: string;
  streamResponseMode?: StreamResponseMode;
  usageEstimated?: boolean;
  usageEstimateReason?: string;
  usageCandidatePath?: string;
  bodyUsageProjectionApplied?: boolean;
}

export interface AccountAttempt {
  attempt: number;
  poolId: number;
  poolName: string;
  outboundModel?: string;
  status?: number;
  action: string;
  durationMs: number;
  errorType?: string;
  errorMessage?: string;
  rawUpstreamError?: RawUpstreamError;
}

export type AccountEventKind =
  | "quota_exhausted"
  | "quota_recovered"
  | "overage_started"
  | "auto_disabled"
  | "risk_controlled"
  | "auto_enabled"
  | "manual_disabled"
  | "manual_enabled"
  | "deleted";

export interface AccountEventSummaryRow {
  event: AccountEventKind | string;
  events: number;
  accounts: number;
}

export interface AccountEventSummaryResponse {
  hours: number;
  since: string;
  accountSource?: string | null;
  items: AccountEventSummaryRow[];
}

export type AccountAuthType = "bearer" | "x_api_key";

export type AccountHeaderProfile =
  | "generic"
  | "anthropic_passthrough"
  | "claude_code_mimic";

export type AccountWireProfile = "default" | "http1_title_case";

export type AccountTlsProfile = "default" | "native_tls";

export type UsageProjectionMode = "pass_through" | "current_path_policy";

export type StreamResponseMode = "event_passthrough";

export type StreamRetryMode =
  | "inherit"
  | "enabled"
  | "disabled"
  | "early_commit";

export type ModelUnavailableCooldownMode = "disabled" | "model" | "pool";

export type AutoDisablePolicy = "inherit" | "disabled" | "enabled";

export interface AccountModelMappingRule {
  enabled?: boolean;
  source: string;
  target: string;
  kind?: "version_equivalent" | "alias" | "fallback";
  note?: string;
}

export interface AccountsConfig {
  enabled: boolean;
  globalMaxConcurrentRequests: number;
  maxQueuedRequests: number;
  capacityMode: "fail_fast" | "wait";
  dispatchMaxWaitSecs: number;
  retryMaxAttempts: number;
  retryStatusCodes: number[];
  retryOnNetworkError: boolean;
  retryOnProtocolError: boolean;
  samePoolRetryCount: number;
  samePoolRetryStatusCodes: number[];
  samePoolRetryDelayMs: number;
  transientFailurePriorityPenalty: number;
  qualityAwareSchedulingEnabled: boolean;
  qualityEwmaAlpha: number;
  qualitySampleTtlSecs: number;
  qualityMinSamples: number;
  qualityPriorityWeight: number;
  qualityLoadWeight: number;
  qualityErrorWeight: number;
  qualityLatencyWeight: number;
  qualityProbationWeight: number;
  qualityTopK: number;
  degradeWindowSecs: number;
  degradeErrorRateThreshold: number;
  degradeProbationSecs: number;
  maxProbationSecs: number;
  probeSharePercent: number;
  recoveryRampSecs: number;
  autoDisableEnabled: boolean;
  autoDisableOnAuthError: boolean;
  autoDisableOnSecurityLock: boolean;
  autoDisableOnQuotaExhausted: boolean;
  autoDisableOnMisconfiguredEndpoint: boolean;
  autoDisableFailureThreshold: number;
  autoDisableWindowSecs: number;
  autoDisableDurationSecs: number;
  rateLimitCooldownSecs: number;
  serverErrorCooldownSecs: number;
  networkErrorCooldownSecs: number;
  protocolErrorCooldownSecs: number;
  modelUnavailableCooldownMode: ModelUnavailableCooldownMode;
  modelUnavailableCooldownSecs: number;
  requestTimeoutSecs: number;
  streamRequestTimeoutSecs: number;
  streamIdleTimeoutSecs: number;
  /** 首个语义输出的最长等待秒数（每个账号尝试单独计时）；超时且未输出时换池，0 表示不限制。 */
  streamFirstOutputTimeoutSecs: number;
  streamPreOutputRetryEnabled: boolean;
  autoDisableOnChannelDisabled: boolean;
  usageProjectionUpliftPercent: number;
  usageProjectionCostFloorEnabled: boolean;
  usageProjectionCostFloorMarginPercent: number;
  usageProjectionOutputUpliftMinTokens: number;
  usageProjectionOutputUpliftPercent: number;
  streamResponseMode: StreamResponseMode;
  usageDebugEnabled: boolean;
  usageDebugDir: string;
  usageDebugMaxBodyBytes: number;
  usageDebugMaxFiles: number;
  wireDebugEnabled: boolean;
  wireDebugDir: string;
  wireDebugMaxBodyBytes: number;
  wireDebugMaxFiles: number;
}

export interface Account {
  tags: string[];
  proxyUrl?: string | null;
  costMultiplier: number;
  overrides: AccountOverrides;
  id: number;
  name: string;
  baseUrl: string;
  apiKey?: string;
  maskedApiKey?: string;
  authType: AccountAuthType;
  headerProfile: AccountHeaderProfile;
  appendBetaQuery: boolean;
  headerOverrides: Record<string, string>;
  wireProfile: AccountWireProfile;
  tlsProfile: AccountTlsProfile;
  enabled: boolean;
  priority: number;
  maxConcurrentRequests: number;
  usageProjectionMode: UsageProjectionMode;
  streamResponseMode?: StreamResponseMode;
  autoDisablePolicy: AutoDisablePolicy;
  preOutputStreamRetryMode: StreamRetryMode;
  autoDisabled: boolean;
  autoDisabledReason?: string;
  autoDisabledAt?: string;
  autoDisabledUntil?: string;
  autoDisabledLastError?: string;
  normalizeModelVersionDots: boolean;
  modelMappingMode: ModelMappingMode;
  modelMappingRequireMatch: boolean;
  modelMappingRules: AccountModelMappingRule[];
  supportedModels: string[];
  routeMode: RouteRuleMode;
  routeRules: string[];
  notes?: string;
  createdAt: string;
  updatedAt: string;
}

export interface AccountsListResponse {
  accounts: Account[];
}

export interface AccountStatus {
  account: Account;
  inFlight: number;
  cooldownRemainingSecs: number;
  cooldownReason?: string;
  transientFailureStreak: number;
  transientFailureTtlSecs: number;
  dispatchable: boolean;
  skippedReason?: string;
  quality?: AccountQualityView;
}

export interface AccountQualityView {
  recentErrorRate: number;
  ttftEwmaMs?: number;
  latencyEwmaMs?: number;
  sampleCount: number;
  scoringActive: boolean;
  inProbation: boolean;
  probationRemainingSecs: number;
  probationLevel: number;
  recoveryProgress: number;
}

export interface AccountsStatusResponse {
  accounts: AccountStatus[];
}

export interface CreateAccountRequest {
  tags?: string[];
  proxyUrl?: string | null;
  costMultiplier?: number;
  overrides?: AccountOverrides;
  name: string;
  baseUrl: string;
  apiKey: string;
  authType?: AccountAuthType;
  headerProfile?: AccountHeaderProfile;
  appendBetaQuery?: boolean;
  headerOverrides?: Record<string, string>;
  wireProfile?: AccountWireProfile;
  tlsProfile?: AccountTlsProfile;
  enabled?: boolean;
  priority?: number;
  maxConcurrentRequests?: number;
  usageProjectionMode?: UsageProjectionMode;
  streamResponseMode?: StreamResponseMode | null;
  autoDisablePolicy?: AutoDisablePolicy;
  preOutputStreamRetryMode?: StreamRetryMode;
  normalizeModelVersionDots?: boolean;
  modelMappingMode?: ModelMappingMode;
  modelMappingRequireMatch?: boolean;
  modelMappingRules?: AccountModelMappingRule[];
  supportedModels?: string[];
  routeMode?: RouteRuleMode;
  routeRules?: string[];
  notes?: string;
}

export interface UpdateAccountRequest {
  tags?: string[];
  proxyUrl?: string | null;
  costMultiplier?: number;
  overrides?: AccountOverrides;
  name?: string;
  baseUrl?: string;
  apiKey?: string;
  authType?: AccountAuthType;
  headerProfile?: AccountHeaderProfile;
  appendBetaQuery?: boolean;
  headerOverrides?: Record<string, string>;
  wireProfile?: AccountWireProfile;
  tlsProfile?: AccountTlsProfile;
  enabled?: boolean;
  priority?: number;
  maxConcurrentRequests?: number;
  usageProjectionMode?: UsageProjectionMode;
  streamResponseMode?: StreamResponseMode | null;
  autoDisablePolicy?: AutoDisablePolicy;
  preOutputStreamRetryMode?: StreamRetryMode;
  normalizeModelVersionDots?: boolean;
  modelMappingMode?: ModelMappingMode;
  modelMappingRequireMatch?: boolean;
  modelMappingRules?: AccountModelMappingRule[];
  supportedModels?: string[];
  routeMode?: RouteRuleMode;
  routeRules?: string[];
  notes?: string;
}

export interface SetSupportedModelsRequest {
  supportedModels: string[];
}

export interface DiscoverAccountSupportedModelsRequest {
  baseUrl?: string | null;
  apiKey?: string | null;
  authType?: "bearer" | "x_api_key" | null;
}

export interface SupportedModelsResponse {
  supportedModels: string[];
  count: number;
}

export interface AccountTestResponse {
  ok: boolean;
  status?: number;
  message: string;
  model?: string;
  response?: string;
}

export interface AccountTestRequest {
  model: string;
  prompt?: string;
}

export interface AccountRoutePolicy extends RoutePolicy {
  id: number;
}

export interface AccountOverrides {
  requestTimeoutSecs?: number | null;
  streamRequestTimeoutSecs?: number | null;
  streamIdleTimeoutSecs?: number | null;
  streamFirstOutputTimeoutSecs?: number | null;
  usageProjectionUpliftPercent?: number | null;
  usageProjectionCostFloorEnabled?: boolean | null;
  usageProjectionCostFloorMarginPercent?: number | null;
  usageProjectionOutputUpliftMinTokens?: number | null;
  usageProjectionOutputUpliftPercent?: number | null;
}
