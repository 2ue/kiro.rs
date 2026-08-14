import { api } from '@/api/http'
import type {
  AdminAuditLogPage,
  AdminAuditLogPageQuery,
  ModelCapabilitiesStatus,
  ManualModelResponse,
  ModelPricingStatus,
  UpsertManualModelRequest,
  UsageAccountBillingByAccount,
  UsageDashboardAccountBillingResponse,
  UsageDashboardResponse,
  UsageDashboardBreakdownResponse,
  UsageDashboardExternalPoolBillingResponse,
  UsageDashboardSeriesResponse,
  UsageDashboardTopResponse,
  UsageDashboardWindowsResponse,
  UsageCleanupPreviewResponse,
  UsageCleanupRequest,
  UsageCleanupStatusResponse,
  UsageExternalPoolRiskQuery,
  UsageExternalPoolRiskResponse,
  UsageRecorderStats,
  UsageRecordsPageQuery,
  UsageRecordsPageResult,
  UsageRecordsQuery,
  UsageRecordsResult,
  UsageSummary,
} from '@/types/api'

function usageQueryParams(query: UsageRecordsQuery | UsageRecordsPageQuery): Record<string, unknown> {
  const accountId = query.accountId ?? query.externalPoolId
  return {
    ...query,
    ...(accountId ? { accountId, externalPoolId: accountId } : {}),
  }
}

function usageRiskQueryParams(query: UsageExternalPoolRiskQuery): Record<string, unknown> {
  const accountId = query.accountId ?? query.externalPoolId
  return {
    ...query,
    ...(accountId ? { accountId, externalPoolId: accountId } : {}),
  }
}

function accountBillingRowsFromExternal(
  rows: UsageDashboardExternalPoolBillingResponse['externalPoolBillingByPool'] = []
): UsageAccountBillingByAccount[] {
  return rows.map(({ poolId, poolName, ...billing }) => ({
    ...billing,
    accountId: poolId,
    accountName: poolName,
  }))
}

function normalizeAccountBillingResponse(
  data: UsageDashboardAccountBillingResponse & Partial<UsageDashboardExternalPoolBillingResponse>
): UsageDashboardAccountBillingResponse {
  return {
    generatedAt: data.generatedAt,
    timezone: data.timezone,
    windowKey: data.windowKey,
    accountBillingByAccount:
      data.accountBillingByAccount ?? accountBillingRowsFromExternal(data.externalPoolBillingByPool),
  }
}

export async function getUsageRecords(query: UsageRecordsQuery = {}): Promise<UsageRecordsResult> {
  const { data } = await api.get<UsageRecordsResult>('/usage-records', { params: usageQueryParams(query) })
  return data
}

export async function getUsageRecordsPage(query: UsageRecordsPageQuery): Promise<UsageRecordsPageResult> {
  const { data } = await api.get<UsageRecordsPageResult>('/usage-records-paged', { params: usageQueryParams(query) })
  return data
}

export async function getUsageSummary(): Promise<UsageSummary> {
  const { data } = await api.get<UsageSummary>('/usage-summary')
  return data
}

export async function getUsageWriterStats(): Promise<UsageRecorderStats> {
  const { data } = await api.get<UsageRecorderStats>('/usage-writer-stats')
  return data
}

export async function getUsageDashboard(timezone = 'Asia/Shanghai'): Promise<UsageDashboardResponse> {
  const { data } = await api.get<UsageDashboardResponse>('/usage-dashboard', {
    params: { timezone },
  })
  return data
}

export async function getUsageDashboardWindows(timezone = 'Asia/Shanghai'): Promise<UsageDashboardWindowsResponse> {
  const { data } = await api.get<UsageDashboardWindowsResponse>('/usage-dashboard/windows', {
    params: { timezone },
  })
  return data
}

export async function getUsageDashboardSeries(timezone = 'Asia/Shanghai'): Promise<UsageDashboardSeriesResponse> {
  const { data } = await api.get<UsageDashboardSeriesResponse>('/usage-dashboard/series', {
    params: { timezone },
  })
  return data
}

export async function getUsageDashboardTop(
  timezone = 'Asia/Shanghai',
  windowKey = 'lifetime'
): Promise<UsageDashboardTopResponse> {
  const { data } = await api.get<UsageDashboardTopResponse>('/usage-dashboard/top', {
    params: { timezone, windowKey },
  })
  return data
}

export async function getUsageDashboardBreakdown(
  timezone = 'Asia/Shanghai',
  windowKey = 'today'
): Promise<UsageDashboardBreakdownResponse> {
  const { data } = await api.get<UsageDashboardBreakdownResponse>('/usage-dashboard/breakdown', {
    params: { timezone, windowKey },
  })
  return data
}

export async function getUsageDashboardExternalPoolBilling(
  timezone = 'Asia/Shanghai',
  windowKey = 'today'
): Promise<UsageDashboardExternalPoolBillingResponse> {
  const { data } = await api.get<UsageDashboardExternalPoolBillingResponse>('/usage-dashboard/external-pool-billing', {
    params: { timezone, windowKey },
  })
  return data
}

export async function getUsageDashboardAccountBilling(
  timezone = 'Asia/Shanghai',
  windowKey = 'today'
): Promise<UsageDashboardAccountBillingResponse> {
  const { data } = await api.get<UsageDashboardAccountBillingResponse & Partial<UsageDashboardExternalPoolBillingResponse>>('/usage-dashboard/account-billing', {
    params: { timezone, windowKey },
  })
  return normalizeAccountBillingResponse(data)
}

export async function getUsageDashboardExternalPoolRisk(
  query: UsageExternalPoolRiskQuery = {}
): Promise<UsageExternalPoolRiskResponse> {
  const { data } = await api.get<UsageExternalPoolRiskResponse>('/usage-dashboard/external-pool-risk', {
    params: usageRiskQueryParams(query),
  })
  return data
}

export async function getUsageDashboardAccountRisk(
  query: UsageExternalPoolRiskQuery = {}
): Promise<UsageExternalPoolRiskResponse> {
  const { data } = await api.get<UsageExternalPoolRiskResponse>('/usage-dashboard/account-risk', {
    params: usageRiskQueryParams(query),
  })
  return data
}

export async function clearUsageRecords(): Promise<UsageCleanupStatusResponse> {
  const { data } = await api.post<UsageCleanupStatusResponse>('/usage-records/clear')
  return data
}

export async function previewUsageCleanup(payload: UsageCleanupRequest): Promise<UsageCleanupPreviewResponse> {
  const { data } = await api.post<UsageCleanupPreviewResponse>('/usage-records/cleanup/preview', payload)
  return data
}

export async function startUsageCleanup(payload: UsageCleanupRequest): Promise<UsageCleanupStatusResponse> {
  const { data } = await api.post<UsageCleanupStatusResponse>('/usage-records/cleanup/start', payload)
  return data
}

export async function getUsageCleanupStatus(): Promise<UsageCleanupStatusResponse> {
  const { data } = await api.get<UsageCleanupStatusResponse>('/usage-records/cleanup/status')
  return data
}

export async function cancelUsageCleanup(): Promise<UsageCleanupStatusResponse> {
  const { data } = await api.post<UsageCleanupStatusResponse>('/usage-records/cleanup/cancel')
  return data
}

export async function resumeUsageCleanup(jobId: string): Promise<UsageCleanupStatusResponse> {
  const { data } = await api.post<UsageCleanupStatusResponse>('/usage-records/cleanup/resume', { jobId })
  return data
}

export async function getAuditLogsPage(query: AdminAuditLogPageQuery): Promise<AdminAuditLogPage> {
  const { data } = await api.get<AdminAuditLogPage>('/audit-logs', { params: query })
  return data
}

export async function getModelPricing(): Promise<ModelPricingStatus> {
  const { data } = await api.get<ModelPricingStatus>('/model-pricing')
  return data
}

export async function syncModelPricing(): Promise<ModelPricingStatus> {
  const { data } = await api.post<ModelPricingStatus>('/model-pricing/sync')
  return data
}

export async function getModelCapabilities(): Promise<ModelCapabilitiesStatus> {
  const { data } = await api.get<ModelCapabilitiesStatus>('/model-capabilities')
  return data
}

export async function syncModelCapabilities(): Promise<ModelCapabilitiesStatus> {
  const { data } = await api.post<ModelCapabilitiesStatus>('/model-capabilities/sync')
  return data
}

export async function upsertManualModel(payload: UpsertManualModelRequest): Promise<ManualModelResponse> {
  const { data } = await api.post<ManualModelResponse>('/model-capabilities/manual', payload)
  return data
}

export async function deleteManualModel(model: string): Promise<ManualModelResponse> {
  const { data } = await api.delete<ManualModelResponse>(`/model-capabilities/manual/${encodeURIComponent(model)}`)
  return data
}
