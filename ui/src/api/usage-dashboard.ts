import { api } from '@/api/http'
import type {
AccountEventSummaryResponse,
AccountEventWindowSummaryQuery,
AdminAuditLogPage,
AdminAuditLogPageQuery,
UsageAccountLatencyResponse,
UsageAccountRiskQuery,
UsageAccountRiskResponse,
UsageCleanupPreviewResponse,
UsageCleanupRequest,
UsageCleanupStatusResponse,
UsageDashboardAccountBillingResponse,
UsageDashboardBreakdownResponse,
UsageDashboardResponse,
UsageDashboardSeriesResponse,
UsageDashboardTopResponse,
UsageDashboardWindowsResponse,
UsageOverviewRankingsResponse,
UsageOverviewResponse,
UsageOverviewSeriesResponse,
UsageOverviewSummaryResponse,
UsageRecorderStats,
UsageRecordsPageQuery,
UsageRecordsPageResult,
UsageRecordsQuery,
UsageRecordsResult,
UsageRouteLatencyResponse,
UsageSummary
} from '@/types/api'

export async function getUsageRecords(query: UsageRecordsQuery = {}): Promise<UsageRecordsResult> {
  const { data } = await api.get<UsageRecordsResult>('/usage-records', { params: query })
  return data
}

export async function getUsageRecordsPage(query: UsageRecordsPageQuery): Promise<UsageRecordsPageResult> {
  const { data } = await api.get<UsageRecordsPageResult>('/usage-records-paged', { params: query })
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

export async function getUsageOverview(params: {
  timezone?: string
  range?: string
  from?: string
  to?: string
  granularity?: 'hour' | 'day'
  topN?: number
} = {}): Promise<UsageOverviewResponse> {
  const { data } = await api.get<UsageOverviewResponse>('/usage-dashboard/overview', {
    params: {
      timezone: 'Asia/Shanghai',
      range: 'today',
      ...params,
    },
  })
  return data
}

type UsageOverviewQuery = {
  timezone?: string
  range?: string
  from?: string
  to?: string
  granularity?: 'hour' | 'day'
  topN?: number
}

function usageOverviewParams(params: UsageOverviewQuery = {}) {
  return {
    timezone: 'Asia/Shanghai',
    range: 'today',
    ...params,
  }
}

export async function getUsageOverviewSummary(params: UsageOverviewQuery = {}): Promise<UsageOverviewSummaryResponse> {
  const { data } = await api.get<UsageOverviewSummaryResponse>('/usage-dashboard/overview/summary', {
    params: usageOverviewParams(params),
  })
  return data
}

export async function getUsageOverviewSeries(params: UsageOverviewQuery = {}): Promise<UsageOverviewSeriesResponse> {
  const { data } = await api.get<UsageOverviewSeriesResponse>('/usage-dashboard/overview/series', {
    params: usageOverviewParams(params),
  })
  return data
}

export async function getUsageOverviewRankings(params: UsageOverviewQuery = {}): Promise<UsageOverviewRankingsResponse> {
  const { data } = await api.get<UsageOverviewRankingsResponse>('/usage-dashboard/overview/rankings', {
    params: usageOverviewParams(params),
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

export async function getUsageDashboardAccountBilling(
  timezone = 'Asia/Shanghai',
  windowKey = 'today'
): Promise<UsageDashboardAccountBillingResponse> {
  const { data } = await api.get<UsageDashboardAccountBillingResponse>('/usage-dashboard/account-billing', {
    params: { timezone, windowKey },
  })
  return data
}

export type UsageAccountLatencyQuery = {
  timezone?: string
  range?: string
  from?: string
  to?: string
  accountId?: number
}

/** 按账号的总耗时 / 首字延迟 p50 / p95 / p99；扫明细，范围最长 8 天。 */
export async function getUsageAccountLatency(params: UsageAccountLatencyQuery = {}): Promise<UsageAccountLatencyResponse> {
  const { data } = await api.get<UsageAccountLatencyResponse>('/usage-dashboard/account-latency', {
    params: { timezone: 'Asia/Shanghai', range: 'today', ...params },
  })
  return data
}

export type UsageRouteLatencyQuery = {
  timezone?: string
  range?: string
  from?: string
  to?: string
}

/** 按路由前缀的总耗时 / 首字延迟 p50 / p95 / p99；扫明细，范围最长 8 天。 */
export async function getUsageRouteLatency(params: UsageRouteLatencyQuery = {}): Promise<UsageRouteLatencyResponse> {
  const { data } = await api.get<UsageRouteLatencyResponse>('/usage-dashboard/route-latency', {
    params: { timezone: 'Asia/Shanghai', range: 'today', ...params },
  })
  return data
}

export async function getUsageDashboardAccountRisk(
  query: UsageAccountRiskQuery = {}
): Promise<UsageAccountRiskResponse> {
  const { data } = await api.get<UsageAccountRiskResponse>('/usage-dashboard/account-risk', {
    params: query,
  })
  return data
}

export async function clearUsageRecords(payload?: UsageCleanupRequest): Promise<UsageCleanupStatusResponse> {
  const { data } = await api.post<UsageCleanupStatusResponse>('/usage-records/clear', payload)
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

export async function getAccountEventSummary(hours: number, accountSource?: string): Promise<AccountEventSummaryResponse> {
  const { data } = await api.get<AccountEventSummaryResponse>('/audit-logs/account-summary', {
    params: { hours, accountSource: accountSource || undefined },
  })
  return data
}

/** 按 `[since, until)` 窗口统计账号异动；给了 since 时后端忽略 hours。 */
export async function getAccountEventWindowSummary(query: AccountEventWindowSummaryQuery): Promise<AccountEventSummaryResponse> {
  const { data } = await api.get<AccountEventSummaryResponse>('/audit-logs/account-summary', {
    params: { since: query.since, until: query.until, accountId: query.accountId || undefined },
  })
  return data
}
