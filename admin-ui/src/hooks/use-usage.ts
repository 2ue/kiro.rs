import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { clearUsageRecords, getUsageRecords, getUsageRecordsPage, getUsageSummary } from '@/api/usage'
import type { UsageRecordsPageQuery, UsageRecordsQuery } from '@/types/api'

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
