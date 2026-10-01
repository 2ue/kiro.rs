import type {
  ExternalPoolAuthType,
  ExternalPoolAutoDisablePolicy,
  ExternalPoolHeaderProfile,
  ExternalPoolModelMappingMode,
  ExternalPoolModelMappingRule,
  ExternalPoolRawModelMode,
  ExternalPoolRequestBodyMode,
  ExternalPoolRouteMode,
  ExternalPoolStreamResponseMode,
  ExternalPoolStreamRetryMode,
  ExternalPoolTlsProfile,
  ExternalPoolUsageProjectionMode,
  ExternalPoolWireProfile,
} from './config'

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
