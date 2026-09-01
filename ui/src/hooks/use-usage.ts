import { useEffect, useRef } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  clearUsageRecords,
  cancelUsageCleanup,
  deleteManualModel,
  getAuditLogsPage,
  getUsageCleanupStatus,
  getModelCapabilities,
  getModelPricing,
  getUsageDashboard,
  getUsageDashboardAccounts,
  getUsageDashboardBreakdown,
  getUsageDashboardExternalPoolBilling,
  getUsageDashboardExternalPoolRisk,
  getUsageDashboardSeries,
  getUsageDashboardTop,
  getUsageDashboardWindows,
  getUsageRecords,
  getUsageRecordsPage,
  getUsageSummary,
  getUsageWriterStats,
  previewUsageCleanup,
  resumeUsageCleanup,
  startUsageCleanup,
  syncModelCapabilities,
  syncModelPricing,
  upsertManualModel,
} from '@/api/usage'
import type { AdminAuditLogPageQuery, UpsertManualModelRequest, UsageCleanupRequest, UsageCleanupStatusResponse, UsageExternalPoolRiskQuery, UsageRecordsPageQuery, UsageRecordsQuery } from '@/types/api'

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

export function useUsageDashboardAccounts(
  params: {
    timezone?: string
    windowKey?: string
    page?: number
    pageSize?: number
    q?: string
    status?: string
    sortBy?: string
    sortOrder?: 'asc' | 'desc'
  } = {},
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-accounts', params],
    queryFn: () => getUsageDashboardAccounts(params),
    refetchInterval,
    enabled,
    placeholderData: (previousData) => previousData,
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

export function useUsageDashboardExternalPoolBilling(
  timezone = 'Asia/Shanghai',
  windowKey = 'today',
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-external-pool-billing', timezone, windowKey],
    queryFn: () => getUsageDashboardExternalPoolBilling(timezone, windowKey),
    refetchInterval,
    enabled,
  })
}

export function useUsageDashboardExternalPoolRisk(
  query: UsageExternalPoolRiskQuery,
  refetchInterval: RefetchInterval = false,
  enabled = true
) {
  return useQuery({
    queryKey: ['usage-dashboard-external-pool-risk', query],
    queryFn: () => getUsageDashboardExternalPoolRisk(query),
    refetchInterval,
    enabled,
  })
}

function invalidateUsageDashboardQueries(queryClient: ReturnType<typeof useQueryClient>) {
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-windows'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-series'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-top'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-accounts'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-breakdown'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-external-pool-billing'] })
  queryClient.invalidateQueries({ queryKey: ['usage-dashboard-external-pool-risk'] })
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

export function useModelPricing(refetchInterval: RefetchInterval = 60000) {
  return useQuery({
    queryKey: ['model-pricing'],
    queryFn: getModelPricing,
    refetchInterval,
  })
}

export function useSyncModelPricing() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: syncModelPricing,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['model-pricing'] }),
  })
}

export function useModelCapabilities() {
  return useQuery({
    queryKey: ['model-capabilities'],
    queryFn: getModelCapabilities,
    refetchInterval: 60000,
  })
}

export function useSyncModelCapabilities() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: syncModelCapabilities,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['model-capabilities'] }),
  })
}

export function useUpsertManualModel() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (payload: UpsertManualModelRequest) => upsertManualModel(payload),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['model-capabilities'] })
      queryClient.invalidateQueries({ queryKey: ['model-pricing'] })
    },
  })
}

export function useDeleteManualModel() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: deleteManualModel,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['model-capabilities'] })
      queryClient.invalidateQueries({ queryKey: ['model-pricing'] })
    },
  })
}
