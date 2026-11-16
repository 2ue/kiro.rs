import { cancelUsageCleanup,clearUsageRecords,getAccountEventSummary,getAuditLogsPage,getUsageCleanupStatus,getUsageDashboard,getUsageDashboardAccountBilling,getUsageDashboardAccountRisk,getUsageDashboardBreakdown,getUsageDashboardSeries,getUsageDashboardTop,getUsageDashboardWindows,getUsageOverview,getUsageOverviewRankings,getUsageOverviewSeries,getUsageOverviewSummary,getUsageRecords,getUsageRecordsPage,getUsageSummary,getUsageWriterStats,previewUsageCleanup,resumeUsageCleanup,startUsageCleanup } from '@/api/usage-dashboard'
import type { AdminAuditLogPageQuery,UsageAccountRiskQuery,UsageCleanupRequest,UsageCleanupStatusResponse,UsageRecordsPageQuery,UsageRecordsQuery } from '@/types/api'
import { useMutation,useQuery,useQueryClient } from '@tanstack/react-query'
import { useEffect,useRef } from 'react'

type RefetchInterval = number | false

export function useUsageRecords(query: UsageRecordsQuery, refetchInterval: RefetchInterval = false) {
  return useQuery({
    queryKey: ['usage-records', query],
    queryFn: () => getUsageRecords(query),
    refetchInterval,
  })
}

export function useUsageRecordsPage(query: UsageRecordsPageQuery, refetchInterval: RefetchInterval = false) {
  return useQuery({
    queryKey: ['usage-records-page', query],
    queryFn: () => getUsageRecordsPage(query),
    refetchInterval,
    placeholderData: (previousData) => previousData,
  })
}

export function useUsageSummary(refetchInterval: RefetchInterval = false) {
  return useQuery({
    queryKey: ['usage-summary'],
    queryFn: getUsageSummary,
    refetchInterval,
  })
}

export function useUsageWriterStats(refetchInterval: RefetchInterval = false) {
  return useQuery({
    queryKey: ['usage-writer-stats'],
    queryFn: getUsageWriterStats,
    refetchInterval,
  })
}

export function useUsageDashboard(timezone = 'Asia/Shanghai', refetchInterval: RefetchInterval = false) {
  return useQuery({
    queryKey: ['usage-dashboard', timezone],
    queryFn: () => getUsageDashboard(timezone),
    refetchInterval,
  })
}

export function useUsageOverview(
  params: {
    timezone?: string
    range?: string
    from?: string
    to?: string
    granularity?: 'hour' | 'day'
    topN?: number
  } = {},
  refetchInterval: RefetchInterval = false,
  enabled = true,
) {
  return useQuery({
    queryKey: ['usage-overview', params],
    queryFn: () => getUsageOverview(params),
    refetchInterval,
    enabled,
    placeholderData: (previousData) => previousData,
  })
}

type UsageOverviewQuery = {
  timezone?: string
  range?: string
  from?: string
  to?: string
  granularity?: 'hour' | 'day'
  topN?: number
}

export function useUsageOverviewSummary(
  params: UsageOverviewQuery = {},
  refetchInterval: RefetchInterval = false,
  enabled = true,
) {
  return useQuery({
    queryKey: ['usage-overview-summary', params],
    queryFn: () => getUsageOverviewSummary(params),
    refetchInterval,
    enabled,
    placeholderData: (previousData) => previousData,
  })
}

export function useUsageOverviewSeries(
  params: UsageOverviewQuery = {},
  refetchInterval: RefetchInterval = false,
  enabled = true,
) {
  return useQuery({
    queryKey: ['usage-overview-series', params],
    queryFn: () => getUsageOverviewSeries(params),
    refetchInterval,
    enabled,
    placeholderData: (previousData) => previousData,
  })
}

export function useUsageOverviewRankings(
  params: UsageOverviewQuery = {},
  refetchInterval: RefetchInterval = false,
  enabled = true,
) {
  return useQuery({
    queryKey: ['usage-overview-rankings', params],
    queryFn: () => getUsageOverviewRankings(params),
    refetchInterval,
    enabled,
    placeholderData: (previousData) => previousData,
  })
}

export function useUsageDashboardWindows(timezone = 'Asia/Shanghai', refetchInterval: RefetchInterval = false) {
  return useQuery({
    queryKey: ['usage-dashboard-windows', timezone],
    queryFn: () => getUsageDashboardWindows(timezone),
    refetchInterval,
  })
}

export function useUsageDashboardSeries(
  timezone = 'Asia/Shanghai',
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-series', timezone],
    queryFn: () => getUsageDashboardSeries(timezone),
    refetchInterval,
    enabled,
  })
}

export function useUsageDashboardTop(
  timezone = 'Asia/Shanghai',
  windowKey = 'lifetime',
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-top', timezone, windowKey],
    queryFn: () => getUsageDashboardTop(timezone, windowKey),
    refetchInterval,
    enabled,
  })
}

export function useUsageDashboardBreakdown(
  timezone = 'Asia/Shanghai',
  windowKey = 'today',
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-breakdown', timezone, windowKey],
    queryFn: () => getUsageDashboardBreakdown(timezone, windowKey),
    refetchInterval,
    enabled,
  })
}

export function useUsageDashboardAccountBilling(
  timezone = 'Asia/Shanghai',
  windowKey = 'today',
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-account-billing', timezone, windowKey],
    queryFn: () => getUsageDashboardAccountBilling(timezone, windowKey),
    refetchInterval,
    enabled,
  })
}

export function useUsageDashboardAccountRisk(
  query: UsageAccountRiskQuery,
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-account-risk', query],
    queryFn: () => getUsageDashboardAccountRisk(query),
    refetchInterval,
    enabled,
  })
}

function invalidateUsageDashboardQueries(queryClient: ReturnType<typeof useQueryClient>) {
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard'] })
  queryClient.invalidateQueries({ queryKey: ['usage-overview'] })
  queryClient.invalidateQueries({ queryKey: ['usage-overview-summary'] })
  queryClient.invalidateQueries({ queryKey: ['usage-overview-series'] })
  queryClient.invalidateQueries({ queryKey: ['usage-overview-local'] })
  queryClient.invalidateQueries({ queryKey: ['usage-overview-external'] })
  queryClient.invalidateQueries({ queryKey: ['usage-overview-rankings'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-windows'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-series'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-top'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-accounts'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-breakdown'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-account-billing'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-account-risk'] })
  queryClient.invalidateQueries({ queryKey: ['usage-writer-stats'] })
}

export function useClearUsageRecords() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (payload?: UsageCleanupRequest) => clearUsageRecords(payload),
    onSuccess: (status) => {
      queryClient.setQueryData(['usage-cleanup-status'], status)
      queryClient.invalidateQueries({ queryKey: ['usage-records'] })
      queryClient.invalidateQueries({ queryKey: ['usage-records-page'] })
      queryClient.invalidateQueries({ queryKey: ['usage-summary'] })
      invalidateUsageDashboardQueries(queryClient)
    },
  })
}

export function useUsageCleanupStatus() {
  return useQuery({
    queryKey: ['usage-cleanup-status'],
    queryFn: getUsageCleanupStatus,
    refetchInterval: (query) => ['queued', 'running'].includes(query.state.data?.status || '') ? 2000 : 10000,
  })
}

export function useRefreshUsageQueriesAfterCleanup(status?: UsageCleanupStatusResponse) {
  const queryClient = useQueryClient()
  const lastInvalidatedKey = useRef<string | null>(null)

  useEffect(() => {
    if (!status?.jobId || ['idle', 'queued', 'running'].includes(status.status)) return
    if ((status.processedRows || 0) <= 0) return

    const invalidationKey = `${status.jobId}:${status.status}:${status.processedRows}`
    if (lastInvalidatedKey.current === invalidationKey) return
    lastInvalidatedKey.current = invalidationKey

    queryClient.invalidateQueries({ queryKey: ['usage-records'] })
    queryClient.invalidateQueries({ queryKey: ['usage-records-page'] })
    queryClient.invalidateQueries({ queryKey: ['usage-summary'] })
    invalidateUsageDashboardQueries(queryClient)
  }, [queryClient, status?.jobId, status?.processedRows, status?.status])
}

export function usePreviewUsageCleanup() {
  return useMutation({
    mutationFn: (payload: UsageCleanupRequest) => previewUsageCleanup(payload),
  })
}

export function useStartUsageCleanup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (payload: UsageCleanupRequest) => startUsageCleanup(payload),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['usage-cleanup-status'] })
      queryClient.invalidateQueries({ queryKey: ['usage-records-page'] })
      queryClient.invalidateQueries({ queryKey: ['usage-summary'] })
      invalidateUsageDashboardQueries(queryClient)
    },
  })
}

export function useCancelUsageCleanup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: cancelUsageCleanup,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['usage-cleanup-status'] }),
  })
}

export function useResumeUsageCleanup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (jobId: string) => resumeUsageCleanup(jobId),
    onSuccess: (status) => queryClient.setQueryData(['usage-cleanup-status'], status),
  })
}

export function useAuditLogsPage(query: AdminAuditLogPageQuery) {
  return useQuery({
    queryKey: ['audit-logs-page', query],
    queryFn: () => getAuditLogsPage(query),
    refetchInterval: 15000,
    placeholderData: (previousData) => previousData,
  })
}

export function useAccountEventSummary(hours: number, accountSource?: string) {
  return useQuery({
    queryKey: ['account-event-summary', hours, accountSource ?? ''],
    queryFn: () => getAccountEventSummary(hours, accountSource),
    refetchInterval: 30000,
    placeholderData: (previousData) => previousData,
  })
}

export { useDeleteManualModel,useModelCapabilities,useModelPricing,useSyncModelPricing,useUpsertManualModel } from './use-models'
