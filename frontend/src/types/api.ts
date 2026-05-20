// =========================================================
// kiro.rs Admin API 类型定义
// =========================================================

export type LoadBalancingMode = 'priority' | 'balanced'
export type SchedulingStatus =
  | 'healthy'
  | 'disabled'
  | 'rate_limited'
  | 'quota_cooldown'
  | 'temp_unschedulable'
  | 'manual_recovery_required'

// ----- 凭据 -----

export interface CredentialStatusItem {
  id: number
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
  successCount: number
  lastUsedAt: string | null
  hasProxy: boolean
  proxyUrl?: string
  refreshFailureCount: number
  disabledReason?: string
  endpoint: string
  schedulingStatus: SchedulingStatus
  schedulingReason?: string
  schedulingUntil?: string
  lastUpstreamStatus?: number
  rateLimitedCount?: number
}

export interface CredentialsStatusResponse {
  total: number
  available: number
  currentId: number
  credentials: CredentialStatusItem[]
}

export interface CredentialsPageResponse extends CredentialsStatusResponse {
  page: number
  limit: number
  totalPages: number
}

export interface CredentialsPageQuery {
  page: number
  limit: number
}

export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
}

export interface SuccessResponse {
  success: boolean
  message: string
}

export interface AddCredentialRequest {
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'api_key'
  clientId?: string
  clientSecret?: string
  email?: string
  priority?: number
  region?: string
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  kiroApiKey?: string
  endpoint?: string
}

export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
}

export interface CredentialTestRequest {
  model: string
  prompt?: string
}

export interface CredentialTestResponse {
  success: boolean
  credentialId: number
  model: string
  statusCode?: number
  outputText?: string
  errorType?: string
  errorMessage?: string
  durationMs: number
  contentType?: string
  rawPreview?: string
}

// ----- 用量记录 -----

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

export interface UsageRecord {
  id: string
  createdAt: string
  endpoint: string
  stream: boolean
  model: string
  conversationId?: string
  credentialId?: number
  credentialLabel?: string
  attemptedCredentialIds?: number[]
  rateLimitedCredentialIds?: number[]
  lastAttemptedCredentialId?: number
  schedulerBlocked?: boolean
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
  durationMs: number
  simulated: boolean
  stickyBound: boolean
  fallbackFromSticky: boolean
  errorType?: string
  errorMessage?: string
  // 自 v2026.4
  clientUserAgent?: string
  clientIp?: string
  requestId?: string
  costUsd?: number | null
}

export interface UsageRecordsQuery {
  limit?: number
  q?: string
  conversationId?: string
  credentialId?: number
  model?: string
  status?: UsageRecordStatus
  source?: UsageSource
  stream?: boolean
  minCacheRead?: number
  since?: string
  until?: string
}

export interface UsageRecordsPageQuery extends UsageRecordsQuery {
  page: number
  limit: number
}

export interface UsageRecordsResult {
  total: number
  records: UsageRecord[]
}

export interface UsageRecordsPageResult extends UsageRecordsResult {
  page: number
  limit: number
  totalPages: number
}

export interface UsageAggregate {
  key: string
  label?: string
  requests: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
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
  localPromptCacheRequests: number
  localPromptCacheInputTokens: number
  localPromptCacheReadInputTokens: number
  localPromptCacheCreationInputTokens: number
  simulatedRequests: number
  upstreamMetadataRequests: number
  topCredentials: UsageAggregate[]
  topConversations: UsageAggregate[]
}

// ----- 模型计价 -----

export interface ModelPrice {
  modelId: string
  displayName?: string
  provider: string
  inputCostPerToken?: number | null
  outputCostPerToken?: number | null
  cacheReadInputTokenCost?: number | null
  cacheCreationInputTokenCost?: number | null
  maxInputTokens?: number | null
  maxOutputTokens?: number | null
  source: string
  syncedAt: string
}

export interface PricingSyncSummary {
  source: string
  fetchedCount: number
  upserted: number
  anthropicOnlyFiltered: number
  startedAt: string
  finishedAt: string
  usedFallback: boolean
}

// ----- 在线运行时配置 -----

export interface AppConfigEntry {
  key: string
  value: unknown
  description?: string
  updatedBy: string
  updatedAt: string
}

// ----- SQL 聚合统计 (GET /api/admin/usage-stats) -----

export interface UsageStatsByModel {
  model: string
  requests: number
  tokens: number
  outputTokens: number
  costUsd: number
}

export interface UsageStatsByCredential {
  credentialId: number
  requests: number
  tokens: number
  outputTokens: number
  costUsd: number
}

export interface UsageStatsBucket {
  bucket: string // ISO timestamp
  requests: number
  tokens: number
  outputTokens: number
  costUsd: number
}

export interface UsageStats {
  /** 今日(无视范围) */
  todayRequests: number
  todayTokens: number
  todayOutputTokens: number
  todayCostUsd: number
  /** 历史累计(无视范围) */
  totalRequests: number
  totalTokens: number
  totalOutputTokens: number
  totalCostUsd: number
  /** 时间范围内 */
  rangeRequests: number
  rangeTokens: number
  rangeOutputTokens: number
  rangeCostUsd: number
  rangeSince: string
  rangeUntil: string
  /** 时间序列(范围内 hour/day 聚合) */
  timeline: UsageStatsBucket[]
  bucket: 'hour' | 'day'
  /** 范围内分组聚合 */
  byModel: UsageStatsByModel[]
  byCredential: UsageStatsByCredential[]
}

/** /usage-stats 查询参数 */
export interface UsageStatsQuery {
  q?: string
  conversationId?: string
  credentialId?: number
  model?: string
  status?: string
  source?: string
  stream?: boolean
  minCacheRead?: number
  since?: string
  until?: string
  bucket?: 'hour' | 'day'
}
