export interface ValidateExistingCredentialsRequest {
  scope?: 'all' | 'enabled' | 'disabled' | 'selected'
  ids?: number[]
  force?: boolean
}

export interface ValidateExternalCredentialsRequest {
  credentials: AddCredentialRequest[]
  querySubscription?: boolean
  queryUsage?: boolean
  checkLiveness?: boolean
  livenessModel?: string
  livenessPrompt?: string
}

export interface CredentialValidationInfo {
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  usagePercentage: number
  checkedAt: string
}

export interface CredentialValidationItem {
  id?: number | null
  index?: number | null
  email?: string | null
  disabled?: boolean | null
  ok: boolean
  previous?: CredentialValidationInfo | null
  current?: CredentialValidationInfo | null
  changeKind: string
  subscriptionKey: string
  subscriptionTitle: string
  error?: string | null
  subscriptionChecked?: boolean
  usageChecked?: boolean
  livenessChecked?: boolean
  subscriptionOk?: boolean | null
  usageOk?: boolean | null
  livenessOk?: boolean | null
  usageError?: string | null
  livenessError?: string | null
  livenessModel?: string | null
  livenessResponse?: string | null
  matchedExistingCredentialId?: number | null
  existingDisabled?: boolean | null
}

export interface CredentialValidationGroup {
  key: string
  title: string
  count: number
  items: CredentialValidationItem[]
}

export interface CredentialValidationResponse {
  total: number
  success: number
  failed: number
  downgraded: number
  upgraded: number
  unchanged: number
  groups: CredentialValidationGroup[]
}

export interface AddCredentialRequest {
  accessToken?: string
  expiresAt?: string
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'external_idp' | 'api_key'
  provider?: string
  clientId?: string
  clientSecret?: string
  tokenEndpoint?: string
  issuerUrl?: string
  scopes?: string
  email?: string
  profileArn?: string
  priority?: number
  maxConcurrentRequests?: number | null
  rpm?: number | null
  rateLimitAutoDisableEnabled?: boolean | null
  disabled?: boolean | null
  enableOverageAfterImport?: boolean | null
  region?: string
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  proxyResourceId?: number | null
  kiroApiKey?: string
  endpoint?: string
  supportedModels?: string[]
  tags?: string[]
  autoDiscoverSupportedModels?: boolean | null
}

export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
  warning?: string
}

export interface TestCredentialRequest {
  model: string
  prompt?: string
}

export interface TestCredentialResponse {
  success: boolean
  credentialId: number
  model: string
  modelId: string
  prompt: string
  response: string
  durationMs: number
}

export interface BatchCredentialImportDefaults {
  disabled?: boolean
  priority?: number
  maxConcurrentRequests?: number | null
  rpm?: number | null
  rateLimitAutoDisableEnabled?: boolean
  provider?: string
  authRegion?: string
  apiRegion?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  proxyResourceId?: number | null
  /** 按账号原始顺序循环绑定的代理资源 ID；非空时优先于 proxyResourceId */
  proxyResourceIds?: number[]
  endpoint?: string
  warmupRemaining?: number
  enableOverageAfterImport?: boolean
  supportedModels?: string[]
  tags?: string[]
}

export interface BatchCredentialImportRequest {
  defaults?: BatchCredentialImportDefaults
  duplicateMode?: 'skip' | 'error'
  continueOnError?: boolean
  autoDiscoverSupportedModels?: boolean
  credentials: AddCredentialRequest[]
}

export interface BatchCredentialImportItem {
  index: number
  ok: boolean
  skipped: boolean
  credentialId?: number
  email?: string
  error?: string
  warning?: string
}

export interface BatchCredentialImportResponse {
  total: number
  success: number
  skipped: number
  failed: number
  items: BatchCredentialImportItem[]
}

export type DisabledReasonCode =
  | 'Manual'
  | 'TooManyFailures'
  | 'TooManyRefreshFailures'
  | 'QuotaExceeded'
  | 'InvalidRefreshToken'
  | 'InvalidConfig'
  | 'TemporarilySuspended'
  | 'AccountSuspended'
  | 'AccountLocked'
