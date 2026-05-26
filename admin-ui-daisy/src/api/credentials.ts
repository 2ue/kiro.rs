import { api } from '@/api/http'
import type {
  AddCredentialRequest,
  AddCredentialResponse,
  BalanceResponse,
  CredentialExportFormat,
  CredentialsPageQuery,
  CredentialsPageResponse,
  CredentialsStatusResponse,
  RuntimeConfig,
  SetDisabledRequest,
  SetPriorityRequest,
  SetWarmupRequest,
  SuccessResponse,
  TestCredentialRequest,
  TestCredentialResponse,
  UpdateRuntimeConfigRequest,
  LoadBalancingMode,
} from '@/types/api'

export async function getCredentials(): Promise<CredentialsStatusResponse> {
  const { data } = await api.get<CredentialsStatusResponse>('/credentials')
  return data
}

export async function getCredentialsPage(query: CredentialsPageQuery): Promise<CredentialsPageResponse> {
  const { data } = await api.get<CredentialsPageResponse>('/credentials-paged', { params: query })
  return data
}

export async function setCredentialDisabled(id: number, disabled: boolean): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/disabled`, { disabled } as SetDisabledRequest)
  return data
}

export async function setCredentialPriority(id: number, priority: number): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/priority`, { priority } as SetPriorityRequest)
  return data
}

export async function setCredentialWarmup(id: number, warmupRemaining: number): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/warmup`, { warmupRemaining } as SetWarmupRequest)
  return data
}

export async function clearCredentialInFlight(id: number, minIdleSecs?: number): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/in-flight/clear`, { minIdleSecs })
  return data
}

export async function resetCredentialFailure(id: number): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/reset`)
  return data
}

export async function forceRefreshToken(id: number): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/refresh`)
  return data
}

export async function getCredentialBalance(id: number): Promise<BalanceResponse> {
  const { data } = await api.get<BalanceResponse>(`/credentials/${id}/balance`)
  return data
}

export async function testCredential(id: number, req: TestCredentialRequest): Promise<TestCredentialResponse> {
  const { data } = await api.post<TestCredentialResponse>(`/credentials/${id}/test`, req)
  return data
}

export async function addCredential(req: AddCredentialRequest): Promise<AddCredentialResponse> {
  const { data } = await api.post<AddCredentialResponse>('/credentials', req)
  return data
}

export async function deleteCredential(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/credentials/${id}`)
  return data
}

export async function getLoadBalancingMode(): Promise<{ mode: LoadBalancingMode }> {
  const { data } = await api.get<{ mode: LoadBalancingMode }>('/config/load-balancing')
  return data
}

export async function setLoadBalancingMode(mode: LoadBalancingMode): Promise<{ mode: LoadBalancingMode }> {
  const { data } = await api.put<{ mode: LoadBalancingMode }>('/config/load-balancing', { mode })
  return data
}

export async function getRuntimeConfig(): Promise<RuntimeConfig> {
  const { data } = await api.get<RuntimeConfig>('/config/runtime')
  return data
}

export async function updateRuntimeConfig(req: UpdateRuntimeConfigRequest): Promise<RuntimeConfig> {
  const { data } = await api.put<RuntimeConfig>('/config/runtime', req)
  return data
}

export async function exportCredentials(format: CredentialExportFormat): Promise<Blob> {
  const { data } = await api.get<Blob>('/credentials/export', {
    params: { format },
    responseType: 'blob',
  })
  return data
}
