import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  AdminAuditLogPage,
  AdminAuditLogPageQuery,
  ManualModelResponse,
  ModelCapabilitiesStatus,
  ModelPricingStatus,
  UpsertManualModelRequest,
  UsageDashboardResponse,
  UsageDashboardBreakdownResponse,
  UsageDashboardExternalPoolBillingResponse,
  UsageDashboardSeriesResponse,
  UsageDashboardTopResponse,
  UsageDashboardWindowsResponse,
  UsageCleanupPreviewResponse,
  UsageCleanupRequest,
  UsageCleanupStatusResponse,
  UsageRecordsPageQuery,
  UsageRecordsPageResult,
  UsageRecordsQuery,
  UsageRecordsResult,
  UsageSummary,
} from '@/types/api'

const api = axios.create({
  baseURL: '/api/admin',
  headers: {
    'Content-Type': 'application/json',
  },
})

function isAdminAuthFailure(status?: number) {
  return status === 401 || status === 403
}

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

export async function getUsageRecords(
  query: UsageRecordsQuery = {}
): Promise<UsageRecordsResult> {
  const { data } = await api.get<UsageRecordsResult>('/usage-records', {
    params: query,
  })
  return data
}

export async function getUsageRecordsPage(
  query: UsageRecordsPageQuery
): Promise<UsageRecordsPageResult> {
  const { data } = await api.get<UsageRecordsPageResult>('/usage-records-paged', {
    params: query,
  })
  return data
}

export async function getUsageSummary(): Promise<UsageSummary> {
  const { data } = await api.get<UsageSummary>('/usage-summary')
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

export async function getAuditLogsPage(
  query: AdminAuditLogPageQuery
): Promise<AdminAuditLogPage> {
  const { data } = await api.get<AdminAuditLogPage>('/audit-logs', {
    params: query,
  })
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
