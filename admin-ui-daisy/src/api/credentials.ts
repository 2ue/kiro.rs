import { api } from '@/api/http'
export { validateAdminApiKey } from '@/api/http'
import type {
  AddCredentialRequest,
  AddCredentialResponse,
  BalanceResponse,
  BatchUpdateCredentialsRequest,
  BatchUpdateCredentialsResponse,
  BulkCredentialActionResponse,
  CredentialAccountInfoListResponse,
  CredentialCreditSummaryResponse,
  CredentialInfoRefreshResponse,
  CredentialExportFormat,
  CredentialListItem,
  CredentialListResponse,
  CredentialRuntimeResponse,
  CredentialSummaryResponse,
  CredentialStatusItem,
  CredentialUsageSummaryResponse,
  CredentialsPageQuery,
  CredentialsPageResponse,
  CredentialsStatusResponse,
  CreateProxyResourceRequest,
  AccessKeysResponse,
  RuntimeConfig,
  SetCredentialConcurrencyRequest,
  SetCredentialRpmRequest,
  SetCredentialRegionsRequest,
  SetDisabledRequest,
  SetCredentialProxyRequest,
  SetPriorityRequest,
  SetWarmupRequest,
  SuccessResponse,
  SystemVersionResponse,
  TestCredentialRequest,
  TestCredentialResponse,
  ValidateExistingCredentialsRequest,
  ValidateExternalCredentialsRequest,
  CredentialValidationResponse,
  ProxyResource,
  ProxyResourceTestRequest,
  ProxyResourceTestResponse,
  ProxyResourcesResponse,
  UpdateAdminApiKeyRequest,
  CreateRequestApiKeyRequest,
  UpdateRequestApiKeyRequest,
  UpdateProxyResourceRequest,
  UpdateRuntimeConfigRequest,
  LoadBalancingMode,
  CreateExternalPoolRequest,
  ExternalPool,
  ExternalPoolsListResponse,
  ExternalPoolsStatusResponse,
  ExternalPoolTestRequest,
  ExternalPoolTestResponse,
  UpdateExternalPoolRequest,
} from '@/types/api'

const CREDENTIALS_LIST_PAGE_LIMIT = 500

function credentialListItemToStatus(item: CredentialListItem): CredentialStatusItem {
  return {
    ...item,
    failureCount: 0,
    isCurrent: false,
    expiresAt: null,
    accountInfo: undefined,
    successCount: 0,
    lastUsedAt: null,
    refreshFailureCount: 0,
    cooledDown: false,
    cooldownRemainingSecs: 0,
    cooldowns: [],
    rateLimited: false,
    rateLimitRemainingSecs: 0,
    inFlightRequests: 0,
    oldestInFlightAgeSecs: 0,
    newestInFlightIdleSecs: 0,
    maxConcurrentRequests: item.maxConcurrentRequests,
    inFlightLeaseMaxSecs: 0,
    warmupRemaining: item.warmupRemaining ?? 0,
    transientFailureStreak: 0,
    recentErrorRate: 0,
    latencyEwmaMs: null,
    inProbation: false,
    probationRemainingSecs: 0,
    schedulerSelectionCount: 0,
    recentSchedulerSelectionCount10s: 0,
    recentSchedulerSelectionCount60s: 0,
    recentSchedulerSelectionCount5m: 0,
    schedulerSelectionPressure: 0,
    schedulerScore: 0,
    estimatedCostUsd: 0,
    pricedRequests: 0,
    unpricedRequests: 0,
  }
}

export async function getCredentials(): Promise<CredentialsStatusResponse> {
  const first = await getCredentialList({ page: 1, limit: CREDENTIALS_LIST_PAGE_LIMIT })
  const items = [...first.items]
  for (let page = 2; page <= first.totalPages; page += 1) {
    const next = await getCredentialList({ page, limit: CREDENTIALS_LIST_PAGE_LIMIT })
    items.push(...next.items)
  }

  return {
    total: first.total,
    available: first.available,
    currentId: 0,
    globalInFlightRequests: 0,
    queuedRequests: 0,
    globalMaxConcurrentRequests: 0,
    maxQueuedRequests: 0,
    credentials: items.map(credentialListItemToStatus),
  }
}

export async function getCredentialsPage(query: CredentialsPageQuery): Promise<CredentialsPageResponse> {
  const { data } = await api.get<CredentialsPageResponse>('/credentials-paged', { params: query })
  return data
}

export async function getCredentialList(query: CredentialsPageQuery): Promise<CredentialListResponse> {
  const { data } = await api.get<CredentialListResponse>('/credentials/list', { params: query })
  return data
}

export async function getCredentialSummary(): Promise<CredentialSummaryResponse> {
  const { data } = await api.get<CredentialSummaryResponse>('/credentials/summary')
  return data
}

export async function getCredentialRuntime(ids: number[]): Promise<CredentialRuntimeResponse> {
  const { data } = await api.get<CredentialRuntimeResponse>('/credentials/runtime', {
    params: { ids: ids.join(',') },
  })
  return data
}

export async function getCredentialAccountInfo(ids: number[]): Promise<CredentialAccountInfoListResponse> {
  const { data } = await api.get<CredentialAccountInfoListResponse>('/credentials/account-info', {
    params: { ids: ids.join(',') },
  })
  return data
}

export async function getCredentialUsageSummary(ids: number[]): Promise<CredentialUsageSummaryResponse> {
  const { data } = await api.get<CredentialUsageSummaryResponse>('/credentials/usage-summary', {
    params: { ids: ids.join(',') },
  })
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

export async function setCredentialConcurrency(id: number, req: SetCredentialConcurrencyRequest): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/concurrency`, req)
  return data
}

export async function setCredentialRpm(id: number, req: SetCredentialRpmRequest): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/rpm`, req)
  return data
}

export async function setCredentialRegions(id: number, req: SetCredentialRegionsRequest): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/regions`, req)
  return data
}

export async function batchUpdateCredentials(req: BatchUpdateCredentialsRequest): Promise<BatchUpdateCredentialsResponse> {
  const { data } = await api.post<BatchUpdateCredentialsResponse>('/credentials/batch-update', req)
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

export async function getCredentialInfo(id: number, force = true): Promise<BalanceResponse> {
  const { data } = await api.get<BalanceResponse>(`/credentials/${id}/info`, { params: { force } })
  return data
}

export async function getCredentialCreditSummary(): Promise<CredentialCreditSummaryResponse> {
  const { data } = await api.get<CredentialCreditSummaryResponse>('/credentials/credit-summary')
  return data
}

export async function refreshCredentialInfo(ids: number[], force = true): Promise<CredentialInfoRefreshResponse> {
  const { data } = await api.post<CredentialInfoRefreshResponse>('/credentials/info/refresh', { ids, force })
  return data
}

export async function validateExistingCredentials(req: ValidateExistingCredentialsRequest): Promise<CredentialValidationResponse> {
  const { data } = await api.post<CredentialValidationResponse>('/credential-validation/existing', req)
  return data
}

export async function validateExternalCredentials(req: ValidateExternalCredentialsRequest): Promise<CredentialValidationResponse> {
  const { data } = await api.post<CredentialValidationResponse>('/credential-validation/external', req)
  return data
}

export async function testCredential(id: number, req: TestCredentialRequest): Promise<TestCredentialResponse> {
  const { data } = await api.post<TestCredentialResponse>(`/credentials/${id}/test`, req)
  return data
}

export async function setCredentialProxy(id: number, req: SetCredentialProxyRequest): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/proxy`, req)
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

export async function deleteDisabledCredentials(): Promise<BulkCredentialActionResponse> {
  const { data } = await api.delete<BulkCredentialActionResponse>('/credentials/disabled')
  return data
}

export async function getProxyResources(): Promise<ProxyResourcesResponse> {
  const { data } = await api.get<ProxyResourcesResponse>('/proxy-resources')
  return data
}

export async function testProxyResourceConfig(req: ProxyResourceTestRequest): Promise<ProxyResourceTestResponse> {
  const { data } = await api.post<ProxyResourceTestResponse>('/proxy-resources/test', req)
  return data
}

export async function testProxyResource(id: number, req: ProxyResourceTestRequest = {}): Promise<ProxyResourceTestResponse> {
  const { data } = await api.post<ProxyResourceTestResponse>(`/proxy-resources/${id}/test`, req)
  return data
}

export async function createProxyResource(req: CreateProxyResourceRequest): Promise<ProxyResource> {
  const { data } = await api.post<ProxyResource>('/proxy-resources', req)
  return data
}

export async function updateProxyResource(id: number, req: UpdateProxyResourceRequest): Promise<ProxyResource> {
  const { data } = await api.put<ProxyResource>(`/proxy-resources/${id}`, req)
  return data
}

export async function deleteProxyResource(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/proxy-resources/${id}`)
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

export async function getSystemVersion(): Promise<SystemVersionResponse> {
  const { data } = await api.get<SystemVersionResponse>('/system/version')
  return data
}

export async function updateRuntimeConfig(req: UpdateRuntimeConfigRequest): Promise<RuntimeConfig> {
  const { data } = await api.put<RuntimeConfig>('/config/runtime', req)
  return data
}

export async function getAccessKeys(): Promise<AccessKeysResponse> {
  const { data } = await api.get<AccessKeysResponse>('/security/keys')
  return data
}

export async function updateAdminApiKey(req: UpdateAdminApiKeyRequest): Promise<AccessKeysResponse> {
  const { data } = await api.put<AccessKeysResponse>('/security/admin-key', req)
  return data
}

export async function createRequestApiKey(req: CreateRequestApiKeyRequest = {}): Promise<AccessKeysResponse> {
  const { data } = await api.post<AccessKeysResponse>('/security/request-keys', req)
  return data
}

export async function updateRequestApiKey(id: string, req: UpdateRequestApiKeyRequest): Promise<AccessKeysResponse> {
  const { data } = await api.put<AccessKeysResponse>(`/security/request-keys/${id}`, req)
  return data
}

export async function deleteRequestApiKey(id: string): Promise<AccessKeysResponse> {
  const { data } = await api.delete<AccessKeysResponse>(`/security/request-keys/${id}`)
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

export async function updateExternalPool(id: number, req: UpdateExternalPoolRequest): Promise<ExternalPool> {
  const { data } = await api.put<ExternalPool>(`/external-pools/${id}`, req)
  return data
}

export async function deleteExternalPool(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/external-pools/${id}`)
  return data
}

export async function setExternalPoolEnabled(id: number, enabled: boolean): Promise<ExternalPool> {
  const { data } = await api.post<ExternalPool>(`/external-pools/${id}/enabled`, { enabled })
  return data
}

export async function clearExternalPoolAutoDisabled(id: number): Promise<ExternalPool> {
  const { data } = await api.post<ExternalPool>(`/external-pools/${id}/auto-disabled/clear`)
  return data
}

export async function testExternalPool(id: number, req?: ExternalPoolTestRequest): Promise<ExternalPoolTestResponse> {
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
