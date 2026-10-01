import { http } from '@/api/client'
import type {
  UsageCleanupPreviewResponse,
  UsageCleanupRequest,
  UsageCleanupStatusResponse,
  UsageDashboardAccountsResponse,
  UsageDashboardBreakdownResponse,
  UsageDashboardExternalPoolBillingResponse,
  UsageDashboardSeriesResponse,
  UsageDashboardTopResponse,
  UsageDashboardWindowsResponse,
  UsageExternalPoolRiskQuery,
  UsageExternalPoolRiskResponse,
  UsageRecorderStats,
  UsageRecordsPageQuery,
  UsageRecordsPageResult,
  UsageSummary,
} from '@/api/types'

/** 浏览器所在时区；与后端窗口口径（今天/本月）对齐 */
export const TIMEZONE = Intl.DateTimeFormat().resolvedOptions().timeZone || 'Asia/Shanghai'

export const usageApi = {
  records: (query: UsageRecordsPageQuery) => http.get<UsageRecordsPageResult>('/usage-records-paged', { ...query }),
  summary: () => http.get<UsageSummary>('/usage-summary'),
  writerStats: () => http.get<UsageRecorderStats>('/usage-writer-stats'),
  windows: () => http.get<UsageDashboardWindowsResponse>('/usage-dashboard/windows', { timezone: TIMEZONE }),
  series: () => http.get<UsageDashboardSeriesResponse>('/usage-dashboard/series', { timezone: TIMEZONE }),
  top: (windowKey: string) => http.get<UsageDashboardTopResponse>('/usage-dashboard/top', { timezone: TIMEZONE, windowKey }),
  breakdown: (windowKey: string) =>
    http.get<UsageDashboardBreakdownResponse>('/usage-dashboard/breakdown', { timezone: TIMEZONE, windowKey }),
  accounts: (params: {
    windowKey: string
    page?: number
    pageSize?: number
    q?: string
    status?: string
    sortBy?: string
    sortOrder?: 'asc' | 'desc'
  }) => http.get<UsageDashboardAccountsResponse>('/usage-dashboard/accounts', { timezone: TIMEZONE, ...params }),
  poolBilling: (windowKey: string) =>
    http.get<UsageDashboardExternalPoolBillingResponse>('/usage-dashboard/external-pool-billing', {
      timezone: TIMEZONE,
      windowKey,
    }),
  poolRisk: (query: UsageExternalPoolRiskQuery) =>
    http.get<UsageExternalPoolRiskResponse>('/usage-dashboard/external-pool-risk', { timezone: TIMEZONE, ...query }),

  cleanupPreview: (req: UsageCleanupRequest) => http.post<UsageCleanupPreviewResponse>('/usage-records/cleanup/preview', req),
  cleanupStart: (req: UsageCleanupRequest) => http.post<UsageCleanupStatusResponse>('/usage-records/cleanup/start', req),
  cleanupStatus: () => http.get<UsageCleanupStatusResponse>('/usage-records/cleanup/status'),
  cleanupCancel: () => http.post<UsageCleanupStatusResponse>('/usage-records/cleanup/cancel'),
  cleanupResume: (jobId: string) => http.post<UsageCleanupStatusResponse>('/usage-records/cleanup/resume', { jobId }),
}
