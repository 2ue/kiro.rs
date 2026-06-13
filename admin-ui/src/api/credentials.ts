import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  CredentialsStatusResponse,
  CredentialsPageQuery,
  CredentialsPageResponse,
  BalanceResponse,
  CredentialInfoRefreshResponse,
  SuccessResponse,
  SetDisabledRequest,
  SetCredentialConcurrencyRequest,
  SetPriorityRequest,
  SetWarmupRequest,
  AddCredentialRequest,
  AddCredentialResponse,
  BatchUpdateCredentialsRequest,
  BatchUpdateCredentialsResponse,
  AccessKeysResponse,
  TestCredentialRequest,
  TestCredentialResponse,
  SetCredentialProxyRequest,
  SetCredentialRegionsRequest,
  RuntimeConfig,
  UpdateAdminApiKeyRequest,
  UpdateRuntimeConfigRequest,
  CredentialExportFormat,
  LoadBalancingMode,
  ProxyResource,
  ProxyResourcesResponse,
  CreateProxyResourceRequest,
  UpdateProxyResourceRequest,
  ValidateExistingCredentialsRequest,
  ValidateExternalCredentialsRequest,
  CredentialValidationResponse,
  CreateExternalPoolRequest,
  ExternalPool,
  ExternalPoolsListResponse,
  ExternalPoolsStatusResponse,
  ExternalPoolTestRequest,
  ExternalPoolTestResponse,
  UpdateExternalPoolRequest,
} from '@/types/api'

// 创建 axios 实例
const api = axios.create({
  baseURL: '/api/admin',
  headers: {
    'Content-Type': 'application/json',
  },
})

function isAdminAuthFailure(status?: number) {
  return status === 401 || status === 403
}

// 请求拦截器添加管理后台 Key（adminApiKey），后端通过 x-api-key 校验。
api.interceptors.request.use((config) => {
  const adminApiKey = storage.getApiKey()
  if (adminApiKey) {
    config.headers['x-api-key'] = adminApiKey
  }
  return config
})

api.interceptors.response.use(
  (response) => response,
  (error) => {
    if (isAdminAuthFailure(error?.response?.status) && typeof window !== 'undefined') {
      window.dispatchEvent(new CustomEvent('kiro-admin-auth-failed'))
    }
    return Promise.reject(error)
  }
)

export async function validateAdminApiKey(adminApiKey: string): Promise<void> {
  await axios.get('/api/admin/config/load-balancing', {
    headers: {
      'x-api-key': adminApiKey,
    },
  })
}

// 获取所有凭据状态
export async function getCredentials(): Promise<CredentialsStatusResponse> {
  const { data } = await api.get<CredentialsStatusResponse>('/credentials')
  return data
}

// 分页获取凭据状态
export async function getCredentialsPage(
  query: CredentialsPageQuery
): Promise<CredentialsPageResponse> {
  const { data } = await api.get<CredentialsPageResponse>('/credentials-paged', {
    params: query,
  })
  return data
}

// 设置凭据禁用状态
export async function setCredentialDisabled(
  id: number,
  disabled: boolean
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/disabled`,
    { disabled } as SetDisabledRequest
  )
  return data
}

// 设置凭据优先级
export async function setCredentialPriority(
  id: number,
  priority: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/priority`,
    { priority } as SetPriorityRequest
  )
  return data
}

export async function setCredentialConcurrency(
  id: number,
  req: SetCredentialConcurrencyRequest
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/concurrency`, req)
  return data
}

export async function setCredentialRegions(
  id: number,
  req: SetCredentialRegionsRequest
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/regions`, req)
  return data
}

export async function batchUpdateCredentials(
  req: BatchUpdateCredentialsRequest
): Promise<BatchUpdateCredentialsResponse> {
  const { data } = await api.post<BatchUpdateCredentialsResponse>('/credentials/batch-update', req)
  return data
}

export async function setCredentialWarmup(
  id: number,
  warmupRemaining: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/warmup`,
    { warmupRemaining } as SetWarmupRequest
  )
  return data
}

export async function clearCredentialInFlight(
  id: number,
  minIdleSecs?: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/in-flight/clear`,
    { minIdleSecs }
  )
  return data
}

// 重置失败计数
export async function resetCredentialFailure(
  id: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/reset`)
  return data
}

// 强制刷新 Token
export async function forceRefreshToken(
  id: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/refresh`)
  return data
}

// 获取凭据额度
export async function getCredentialBalance(id: number): Promise<BalanceResponse> {
  const { data } = await api.get<BalanceResponse>(`/credentials/${id}/balance`)
  return data
}

export async function getCredentialInfo(id: number, force = true): Promise<BalanceResponse> {
  const { data } = await api.get<BalanceResponse>(`/credentials/${id}/info`, {
    params: { force },
  })
  return data
}

export async function refreshCredentialInfo(
  ids: number[],
  force = true
): Promise<CredentialInfoRefreshResponse> {
  const { data } = await api.post<CredentialInfoRefreshResponse>('/credentials/info/refresh', {
    ids,
    force,
  })
  return data
}

export async function validateExistingCredentials(
  req: ValidateExistingCredentialsRequest
): Promise<CredentialValidationResponse> {
  const { data } = await api.post<CredentialValidationResponse>('/credential-validation/existing', req)
  return data
}

export async function validateExternalCredentials(
  req: ValidateExternalCredentialsRequest
): Promise<CredentialValidationResponse> {
  const { data } = await api.post<CredentialValidationResponse>('/credential-validation/external', req)
  return data
}

// 测试指定凭据的模型调用
export async function testCredential(
  id: number,
  req: TestCredentialRequest
): Promise<TestCredentialResponse> {
  const { data } = await api.post<TestCredentialResponse>(`/credentials/${id}/test`, req)
  return data
}

export async function setCredentialProxy(
  id: number,
  req: SetCredentialProxyRequest
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/proxy`, req)
  return data
}

// 添加新凭据
export async function addCredential(
  req: AddCredentialRequest
): Promise<AddCredentialResponse> {
  const { data } = await api.post<AddCredentialResponse>('/credentials', req)
  return data
}

// 删除凭据
export async function deleteCredential(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/credentials/${id}`)
  return data
}

export async function getProxyResources(): Promise<ProxyResourcesResponse> {
  const { data } = await api.get<ProxyResourcesResponse>('/proxy-resources')
  return data
}

export async function createProxyResource(
  req: CreateProxyResourceRequest
): Promise<ProxyResource> {
  const { data } = await api.post<ProxyResource>('/proxy-resources', req)
  return data
}

export async function updateProxyResource(
  id: number,
  req: UpdateProxyResourceRequest
): Promise<ProxyResource> {
  const { data } = await api.put<ProxyResource>(`/proxy-resources/${id}`, req)
  return data
}

export async function deleteProxyResource(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/proxy-resources/${id}`)
  return data
}

// 获取负载均衡模式
export async function getLoadBalancingMode(): Promise<{ mode: LoadBalancingMode }> {
  const { data } = await api.get<{ mode: LoadBalancingMode }>('/config/load-balancing')
  return data
}

// 设置负载均衡模式
export async function setLoadBalancingMode(mode: LoadBalancingMode): Promise<{ mode: LoadBalancingMode }> {
  const { data } = await api.put<{ mode: LoadBalancingMode }>('/config/load-balancing', { mode })
  return data
}

export async function getRuntimeConfig(): Promise<RuntimeConfig> {
  const { data } = await api.get<RuntimeConfig>('/config/runtime')
  return data
}

export async function updateRuntimeConfig(
  req: UpdateRuntimeConfigRequest
): Promise<RuntimeConfig> {
  const { data } = await api.put<RuntimeConfig>('/config/runtime', req)
  return data
}

export async function getAccessKeys(): Promise<AccessKeysResponse> {
  const { data } = await api.get<AccessKeysResponse>('/security/keys')
  return data
}

export async function updateAdminApiKey(
  req: UpdateAdminApiKeyRequest
): Promise<AccessKeysResponse> {
  const { data } = await api.put<AccessKeysResponse>('/security/admin-key', req)
  return data
}

export async function getExternalPools(): Promise<ExternalPoolsListResponse> {
  const { data } = await api.get<ExternalPoolsListResponse>('/external-pools')
  return data
}

export async function getExternalPoolsStatus(): Promise<ExternalPoolsStatusResponse> {
  const { data } = await api.get<ExternalPoolsStatusResponse>('/external-pools/status')
  return data
}

export async function createExternalPool(req: CreateExternalPoolRequest): Promise<ExternalPool> {
  const { data } = await api.post<ExternalPool>('/external-pools', req)
  return data
}

export async function updateExternalPool(
  id: number,
  req: UpdateExternalPoolRequest
): Promise<ExternalPool> {
  const { data } = await api.put<ExternalPool>(`/external-pools/${id}`, req)
  return data
}

export async function deleteExternalPool(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/external-pools/${id}`)
  return data
}

export async function setExternalPoolEnabled(
  id: number,
  enabled: boolean
): Promise<ExternalPool> {
  const { data } = await api.post<ExternalPool>(`/external-pools/${id}/enabled`, { enabled })
  return data
}

export async function clearExternalPoolAutoDisabled(id: number): Promise<ExternalPool> {
  const { data } = await api.post<ExternalPool>(`/external-pools/${id}/auto-disabled/clear`)
  return data
}

export async function testExternalPool(
  id: number,
  req?: ExternalPoolTestRequest
): Promise<ExternalPoolTestResponse> {
  const { data } = await api.post<ExternalPoolTestResponse>(`/external-pools/${id}/test`, req)
  return data
}

export async function exportCredentials(format: CredentialExportFormat): Promise<Blob> {
  const { data } = await api.get<Blob>('/credentials/export', {
    params: { format },
    responseType: 'blob',
  })
  return data
}
