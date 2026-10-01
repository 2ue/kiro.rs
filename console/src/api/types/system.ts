import type { RequestAdmissionConfig } from './config'

export interface AdminAuditLogRow {
  id: number
  createdAt: string
  actor: string
  action: string
  objectType: string
  objectId?: string
  success: boolean
  errorMessage?: string
  detail: unknown
}

export interface AdminAuditLogPage {
  page: number
  limit: number
  hasNext: boolean
  records: AdminAuditLogRow[]
}

export interface AdminAuditLogPageQuery {
  page: number
  limit: number
}

export interface AccessKeysResponse {
  requestApiKey: string
  maskedRequestApiKey: string
  requestApiKeys: RequestApiKeyItem[]
  defaultRequestAdmission: RequestAdmissionConfig
  adminApiKey: string
  maskedAdminApiKey: string
}

export interface RequestApiKeyItem {
  id: string
  apiKey: string
  maskedApiKey: string
  primary: boolean
  name: string
  enabled: boolean
  requestAdmission?: RequestAdmissionConfig
}

export interface CreateRequestApiKeyRequest {
  apiKey?: string
  name?: string
  enabled?: boolean
  requestAdmission?: RequestAdmissionConfig
}

export interface UpdateRequestApiKeyRequest {
  apiKey?: string
  name?: string
  enabled?: boolean
  requestAdmission?: RequestAdmissionConfig
}

export interface UpdateAdminApiKeyRequest {
  adminApiKey: string
}

export type LoadBalancingMode = 'priority' | 'balanced' | 'health_balanced' | 'weighted_least_inflight'

export interface ModelPricing {
  inputCostPerToken: number
  outputCostPerToken: number
  cacheCreationInputTokenCost: number
  cacheReadInputTokenCost: number
}

export interface ModelPriceItem {
  model: string
  pricing: ModelPricing
  source?: string
}

export interface ModelPricingStatus {
  available: boolean
  source: string
  sourceUrl: string
  modelCount: number
  lastSyncedAt?: string
  lastError?: string
  models: ModelPriceItem[]
}

export interface ModelCapabilityItem {
  model: string
  displayName: string
  description?: string
  maxInputTokens?: number
  maxOutputTokens?: number
  supportsPromptCaching?: boolean
  supportedInputTypes: string[]
  source?: string
}

export interface ModelCapabilitiesStatus {
  available: boolean
  source: string
  modelCount: number
  lastSyncedAt?: string
  lastError?: string
  models: ModelCapabilityItem[]
  reasoningFields?: Record<string, {
    path: 'output_config' | 'reasoning'
    efforts: string[]
    defaultEffort?: string
  }>
}

export interface SyncModelCapabilitiesRequest {
  credentialIds?: number[]
}

export interface ManualModelPricingRequest {
  inputCostPerMillion: number
  outputCostPerMillion: number
  cacheCreationInputCostPerMillion?: number
  cacheReadInputCostPerMillion?: number
}

export interface UpsertManualModelRequest {
  model: string
  displayName?: string
  description?: string
  maxInputTokens?: number
  maxOutputTokens?: number
  supportsPromptCaching?: boolean
  supportedInputTypes: string[]
  pricing?: ManualModelPricingRequest
  clearPricing?: boolean
}

export interface ManualModelResponse {
  success: boolean
  message: string
  model: string
}

export type CredentialExportFormat = 'json' | 'backup-json' | 'jsonl'
