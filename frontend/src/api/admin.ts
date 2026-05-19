import { adminApi } from '@/lib/api'
import type {
  AddCredentialRequest,
  AddCredentialResponse,
  AppConfigEntry,
  BalanceResponse,
  CredentialsPageQuery,
  CredentialsPageResponse,
  CredentialsStatusResponse,
  CredentialTestRequest,
  CredentialTestResponse,
  LoadBalancingMode,
  ModelPrice,
  PricingSyncSummary,
  SuccessResponse,
  UsageRecordsPageQuery,
  UsageRecordsPageResult,
  UsageRecordsQuery,
  UsageRecordsResult,
  UsageStats,
  UsageStatsQuery,
  UsageSummary,
} from '@/types/api'

// ===== Credentials =====

export async function getCredentials(): Promise<CredentialsStatusResponse> {
  const { data } = await adminApi.get<CredentialsStatusResponse>('/credentials')
  return data
}

export async function getCredentialsPage(
  query: CredentialsPageQuery,
): Promise<CredentialsPageResponse> {
  const { data } = await adminApi.get<CredentialsPageResponse>('/credentials-paged', {
    params: query,
  })
  return data
}

export async function setCredentialDisabled(
  id: number,
  disabled: boolean,
): Promise<SuccessResponse> {
  const { data } = await adminApi.post<SuccessResponse>(
    `/credentials/${id}/disabled`,
    { disabled },
  )
  return data
}

export async function setCredentialPriority(
  id: number,
  priority: number,
): Promise<SuccessResponse> {
  const { data } = await adminApi.post<SuccessResponse>(
    `/credentials/${id}/priority`,
    { priority },
  )
  return data
}

export async function resetCredentialFailure(id: number): Promise<SuccessResponse> {
  const { data } = await adminApi.post<SuccessResponse>(`/credentials/${id}/reset`)
  return data
}

export async function forceRefreshToken(id: number): Promise<SuccessResponse> {
  const { data } = await adminApi.post<SuccessResponse>(`/credentials/${id}/refresh`)
  return data
}

export async function getCredentialBalance(id: number): Promise<BalanceResponse> {
  const { data } = await adminApi.get<BalanceResponse>(`/credentials/${id}/balance`)
  return data
}

export async function testCredential(
  id: number,
  req: CredentialTestRequest,
): Promise<CredentialTestResponse> {
  const { data } = await adminApi.post<CredentialTestResponse>(
    `/credentials/${id}/test`,
    req,
  )
  return data
}

export async function addCredential(
  req: AddCredentialRequest,
): Promise<AddCredentialResponse> {
  const { data } = await adminApi.post<AddCredentialResponse>('/credentials', req)
  return data
}

export async function deleteCredential(id: number): Promise<SuccessResponse> {
  const { data } = await adminApi.delete<SuccessResponse>(`/credentials/${id}`)
  return data
}

export async function getLoadBalancingMode(): Promise<{ mode: LoadBalancingMode }> {
  const { data } = await adminApi.get<{ mode: LoadBalancingMode }>(
    '/config/load-balancing',
  )
  return data
}

export async function setLoadBalancingMode(
  mode: LoadBalancingMode,
): Promise<{ mode: LoadBalancingMode }> {
  const { data } = await adminApi.put<{ mode: LoadBalancingMode }>(
    '/config/load-balancing',
    { mode },
  )
  return data
}

// ===== Usage =====

export async function getUsageRecords(
  query: UsageRecordsQuery = {},
): Promise<UsageRecordsResult> {
  const { data } = await adminApi.get<UsageRecordsResult>('/usage-records', {
    params: query,
  })
  return data
}

export async function getUsageRecordsPage(
  query: UsageRecordsPageQuery,
): Promise<UsageRecordsPageResult> {
  const { data } = await adminApi.get<UsageRecordsPageResult>('/usage-records-paged', {
    params: query,
  })
  return data
}

export async function getUsageSummary(): Promise<UsageSummary> {
  const { data } = await adminApi.get<UsageSummary>('/usage-summary')
  return data
}

export async function getUsageStats(query: UsageStatsQuery = {}): Promise<UsageStats> {
  const { data } = await adminApi.get<UsageStats>('/usage-stats', { params: query })
  return data
}

export async function clearUsageRecords(): Promise<SuccessResponse> {
  const { data } = await adminApi.post<SuccessResponse>('/usage-records/clear')
  return data
}

// ===== Pricing =====

export async function listPricing(): Promise<ModelPrice[]> {
  const { data } = await adminApi.get<ModelPrice[]>('/pricing')
  return data
}

export async function syncPricing(forceBuiltin = false): Promise<PricingSyncSummary> {
  const { data } = await adminApi.post<PricingSyncSummary>('/pricing/sync', {
    forceBuiltin,
  })
  return data
}

// ===== App Config =====

export async function listAppConfig(): Promise<AppConfigEntry[]> {
  const { data } = await adminApi.get<AppConfigEntry[]>('/config')
  return data
}

export async function updateAppConfig(
  items: Record<string, unknown>,
): Promise<SuccessResponse> {
  const { data } = await adminApi.put<SuccessResponse>('/config', { items })
  return data
}
