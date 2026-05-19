// 凭据状态响应
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

// 单个凭据状态
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
  schedulingStatus: 'healthy' | 'disabled' | 'rate_limited' | 'quota_cooldown'
  schedulingReason?: string
  schedulingUntil?: string
  lastUpstreamStatus?: number
  rateLimitedCount?: number
}

// 余额响应
export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
}

// 成功响应
export interface SuccessResponse {
  success: boolean
  message: string
}

// 错误响应
export interface AdminErrorResponse {
  error: {
    type: string
    message: string
  }
}

// 请求类型
export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

// 添加凭据请求
export interface AddCredentialRequest {
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'api_key'
  clientId?: string
  clientSecret?: string
  email?: string
  priority?: number
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  kiroApiKey?: string
  endpoint?: string
}

// 添加凭据响应
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
}

export interface UsageRecordsPageQuery extends UsageRecordsQuery {
  page: number
  limit: number
}
