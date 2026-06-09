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
  getUsageRecords,
  getUsageRecordsPage,
  getUsageSummary,
  previewUsageCleanup,
  startUsageCleanup,
  syncModelCapabilities,
  syncModelPricing,
  upsertManualModel,
} from '@/api/usage'
import type { AdminAuditLogPageQuery, UpsertManualModelRequest, UsageCleanupRequest, UsageRecordsPageQuery, UsageRecordsQuery } from '@/types/api'

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

export function useUsageDashboard(timezone = 'Asia/Shanghai', refetchInterval: RefetchInterval = false) {
  return useQuery({
    queryKey: ['usage-dashboard', timezone],
    queryFn: () => getUsageDashboard(timezone),
    refetchInterval,
  })
}

export function useClearUsageRecords() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: clearUsageRecords,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['usage-records'] })
      queryClient.invalidateQueries({ queryKey: ['usage-records-page'] })
      queryClient.invalidateQueries({ queryKey: ['usage-summary'] })
      queryClient.invalidateQueries({ queryKey: ['usage-dashboard'] })
    },
  })
}

export function useUsageCleanupStatus() {
  return useQuery({
    queryKey: ['usage-cleanup-status'],
    queryFn: getUsageCleanupStatus,
    refetchInterval: (query) => query.state.data?.status === 'running' ? 2000 : 10000,
  })
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
      queryClient.invalidateQueries({ queryKey: ['usage-dashboard'] })
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
