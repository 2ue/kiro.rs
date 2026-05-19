import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  clearUsageRecords,
  getUsageRecordsPage,
  getUsageStats,
  getUsageSummary,
} from '@/api/admin'
import type { UsageRecordsPageQuery, UsageStatsQuery } from '@/types/api'

export function useUsageRecordsPage(query: UsageRecordsPageQuery) {
  return useQuery({
    queryKey: ['usage-records-page', query],
    queryFn: () => getUsageRecordsPage(query),
    refetchInterval: 10_000,
    placeholderData: (prev) => prev,
  })
}

export function useUsageSummary() {
  return useQuery({
    queryKey: ['usage-summary'],
    queryFn: getUsageSummary,
    refetchInterval: 10_000,
  })
}

/**
 * 后端 SQL 聚合统计。**与列表分页无关**,只受过滤参数 + 时间范围影响 —
 * 切换页码 / 页大小不会影响这里返回的数据。
 */
export function useUsageStats(filter: UsageStatsQuery = {}) {
  return useQuery({
    queryKey: ['usage-stats', filter],
    queryFn: () => getUsageStats(filter),
    refetchInterval: 10_000,
    placeholderData: (prev) => prev,
  })
}

export function useClearUsageRecords() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: clearUsageRecords,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['usage-records-page'] })
      qc.invalidateQueries({ queryKey: ['usage-summary'] })
      qc.invalidateQueries({ queryKey: ['usage-stats'] })
    },
  })
}
