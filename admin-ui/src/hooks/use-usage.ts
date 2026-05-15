import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { clearUsageRecords, getUsageRecords, getUsageSummary } from '@/api/usage'
import type { UsageRecordsQuery } from '@/types/api'

export function useUsageRecords(query: UsageRecordsQuery) {
  return useQuery({
    queryKey: ['usage-records', query],
    queryFn: () => getUsageRecords(query),
    refetchInterval: 10000,
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
      queryClient.invalidateQueries({ queryKey: ['usage-summary'] })
    },
  })
}
