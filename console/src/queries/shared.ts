import { useQuery } from '@tanstack/react-query'
import { credentialsApi } from '@/api/endpoints/credentials'
import { poolsApi } from '@/api/endpoints/pools'
import { proxiesApi } from '@/api/endpoints/proxies'
import { systemApi } from '@/api/endpoints/system'
import { usageApi } from '@/api/endpoints/usage'
import { usePollInterval } from '@/stores/ui-prefs'
import { qk } from './keys'

export function useCredentialSummary() {
  return useQuery({
    queryKey: qk.credentials.summary,
    queryFn: credentialsApi.summary,
    refetchInterval: usePollInterval('fast'),
  })
}

export function useWriterStats() {
  return useQuery({
    queryKey: qk.usage.writer,
    queryFn: usageApi.writerStats,
    refetchInterval: usePollInterval('normal'),
  })
}

export function useSystemVersion() {
  return useQuery({ queryKey: qk.system.version, queryFn: systemApi.version, staleTime: Infinity })
}

export function usePoolsStatus() {
  return useQuery({
    queryKey: qk.pools.status,
    queryFn: poolsApi.status,
    refetchInterval: usePollInterval('fast'),
  })
}

export function useLoadBalancing() {
  return useQuery({ queryKey: qk.system.loadBalancing, queryFn: systemApi.loadBalancing, staleTime: 60_000 })
}

export function useRuntimeConfig() {
  return useQuery({ queryKey: qk.system.runtimeConfig, queryFn: systemApi.runtimeConfig, staleTime: 30_000 })
}

export function useProxies() {
  return useQuery({ queryKey: qk.proxies.list, queryFn: proxiesApi.list, staleTime: 30_000 })
}

export function useModelCapabilities() {
  return useQuery({ queryKey: qk.system.capabilities, queryFn: systemApi.modelCapabilities, staleTime: 5 * 60_000 })
}

export function useModelPricing() {
  return useQuery({ queryKey: qk.system.pricing, queryFn: systemApi.modelPricing, staleTime: 5 * 60_000 })
}

export function useUsageWindows() {
  return useQuery({
    queryKey: qk.usage.windows,
    queryFn: usageApi.windows,
    refetchInterval: usePollInterval('normal'),
  })
}

export function useUsageSeries() {
  return useQuery({
    queryKey: qk.usage.series,
    queryFn: usageApi.series,
    refetchInterval: usePollInterval('normal'),
  })
}

export function useUsageTop(windowKey: string) {
  return useQuery({
    queryKey: qk.usage.top(windowKey),
    queryFn: () => usageApi.top(windowKey),
    refetchInterval: usePollInterval('normal'),
  })
}

export function usePoolsList() {
  return useQuery({ queryKey: qk.pools.list, queryFn: poolsApi.list, staleTime: 30_000 })
}
