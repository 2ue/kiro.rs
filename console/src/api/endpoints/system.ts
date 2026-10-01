import { http } from '@/api/client'
import type {
  AccessKeysResponse,
  AdminAuditLogPage,
  CreateRequestApiKeyRequest,
  LoadBalancingMode,
  ManualModelResponse,
  ModelCapabilitiesStatus,
  ModelPricingStatus,
  RuntimeConfig,
  SyncModelCapabilitiesRequest,
  SystemVersionResponse,
  UpdateRequestApiKeyRequest,
  UpsertManualModelRequest,
} from '@/api/types'

export const systemApi = {
  /** 用待验证的 Key 请求版本接口，用于登录校验 */
  verifyKey: (apiKey: string) => http.get<SystemVersionResponse>('/system/version', undefined, { apiKey }),
  version: () => http.get<SystemVersionResponse>('/system/version'),

  loadBalancing: () => http.get<{ mode: LoadBalancingMode }>('/config/load-balancing'),
  setLoadBalancing: (mode: LoadBalancingMode) => http.put<{ mode: LoadBalancingMode }>('/config/load-balancing', { mode }),
  runtimeConfig: () => http.get<RuntimeConfig>('/config/runtime'),
  updateRuntimeConfig: (config: RuntimeConfig) => http.put<RuntimeConfig>('/config/runtime', config),

  accessKeys: () => http.get<AccessKeysResponse>('/security/keys'),
  updateAdminKey: (adminApiKey: string) => http.put<AccessKeysResponse>('/security/admin-key', { adminApiKey }),
  createRequestKey: (req: CreateRequestApiKeyRequest) => http.post<AccessKeysResponse>('/security/request-keys', req),
  updateRequestKey: (id: string, req: UpdateRequestApiKeyRequest) =>
    http.put<AccessKeysResponse>(`/security/request-keys/${encodeURIComponent(id)}`, req),
  deleteRequestKey: (id: string) => http.delete<AccessKeysResponse>(`/security/request-keys/${encodeURIComponent(id)}`),

  auditLogs: (page: number, limit: number) => http.get<AdminAuditLogPage>('/audit-logs', { page, limit }),

  modelPricing: () => http.get<ModelPricingStatus>('/model-pricing'),
  syncModelPricing: () => http.post<ModelPricingStatus>('/model-pricing/sync'),
  modelCapabilities: () => http.get<ModelCapabilitiesStatus>('/model-capabilities'),
  syncModelCapabilities: (req: SyncModelCapabilitiesRequest = {}) =>
    http.post<ModelCapabilitiesStatus>('/model-capabilities/sync', req),
  upsertManualModel: (req: UpsertManualModelRequest) => http.post<ManualModelResponse>('/model-capabilities/manual', req),
  deleteManualModel: (model: string) =>
    http.delete<ManualModelResponse>(`/model-capabilities/manual/${encodeURIComponent(model)}`),
}
