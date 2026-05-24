import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  clearUsageRecords,
  getAuditLogsPage,
  getModelCapabilities,
  getModelPricing,
  getUsageRecords,
  getUsageRecordsPage,
  getUsageSummary,
  syncModelPricing,
  syncModelCapabilities,
} from '@/api/usage'
import type { AdminAuditLogPageQuery, UsageRecordsPageQuery, UsageRecordsQuery } from '@/types/api'

export function useUsageRecords(query: UsageRecordsQuery) {
  return useQuery({
    queryKey: ['usage-records', query],
    queryFn: () => getUsageRecords(query),
    refetchInterval: 10000,
  })
}

export function useUsageRecordsPage(query: UsageRecordsPageQuery) {
  return useQuery({
    queryKey: ['usage-records-page', query],
    queryFn: () => getUsageRecordsPage(query),
    refetchInterval: 10000,
    placeholderData: (previousData) => previousData,
  })
}

export function useUsageSummary() {
  return useQuery({
    queryKey: ['usage-summary'],
    queryFn: getUsageSummary,
    refetchInterval: 10000,
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
    },
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

export function useModelPricing() {
  return useQuery({
    queryKey: ['model-pricing'],
    queryFn: getModelPricing,
    refetchInterval: 60000,
  })
}

export function useSyncModelPricing() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: syncModelPricing,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['model-pricing'] })
    },
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
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['model-capabilities'] })
    },
  })
}
