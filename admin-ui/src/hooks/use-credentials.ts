import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  getCredentials,
  getCredentialsPage,
  setCredentialDisabled,
  setCredentialPriority,
  setCredentialWarmup,
  clearCredentialInFlight,
  resetCredentialFailure,
  forceRefreshToken,
  getCredentialBalance,
  testCredential,
  addCredential,
  deleteCredential,
  getLoadBalancingMode,
  setLoadBalancingMode,
  getRuntimeConfig,
  updateRuntimeConfig,
} from '@/api/credentials'
import type { AddCredentialRequest, CredentialsPageQuery, TestCredentialRequest, UpdateRuntimeConfigRequest } from '@/types/api'

function invalidateCredentialCaches(queryClient: ReturnType<typeof useQueryClient>, id?: number) {
  queryClient.invalidateQueries({ queryKey: ['credentials'] })
  queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
  if (typeof id === 'number') {
    queryClient.invalidateQueries({ queryKey: ['credential-balance', id] })
  } else {
    queryClient.invalidateQueries({ queryKey: ['credential-balance'] })
  }
}

interface UseCredentialsOptions {
  enabled?: boolean
}

// 查询凭据列表
export function useCredentials(options: UseCredentialsOptions = {}) {
  return useQuery({
    queryKey: ['credentials'],
    queryFn: getCredentials,
    enabled: options.enabled ?? true,
    refetchInterval: 30000, // 每 30 秒刷新一次
  })
}

// 分页查询凭据列表
export function useCredentialsPage(query: CredentialsPageQuery) {
  return useQuery({
    queryKey: ['credentials-page', query],
    queryFn: () => getCredentialsPage(query),
    refetchInterval: 30000,
    placeholderData: (previousData) => previousData,
  })
}

// 查询凭据额度
export function useCredentialBalance(id: number | null) {
  return useQuery({
    queryKey: ['credential-balance', id],
    queryFn: () => getCredentialBalance(id!),
    enabled: id !== null,
    retry: false, // 额度查询失败时不重试（避免重复请求被封禁的账号）
  })
}

// 测试指定凭据的模型调用
export function useTestCredential() {
  return useMutation({
    mutationFn: ({ id, request }: { id: number; request: TestCredentialRequest }) =>
      testCredential(id, request),
  })
}

// 设置禁用状态
export function useSetDisabled() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, disabled }: { id: number; disabled: boolean }) =>
      setCredentialDisabled(id, disabled),
    onSuccess: (_data, variables) => {
      invalidateCredentialCaches(queryClient, variables.id)
    },
  })
}

// 设置优先级
export function useSetPriority() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, priority }: { id: number; priority: number }) =>
      setCredentialPriority(id, priority),
    onSuccess: (_data, variables) => {
      invalidateCredentialCaches(queryClient, variables.id)
    },
  })
}

export function useSetWarmup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, warmupRemaining }: { id: number; warmupRemaining: number }) =>
      setCredentialWarmup(id, warmupRemaining),
    onSuccess: (_data, variables) => {
      invalidateCredentialCaches(queryClient, variables.id)
    },
  })
}

export function useClearInFlight() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, minIdleSecs }: { id: number; minIdleSecs?: number }) =>
      clearCredentialInFlight(id, minIdleSecs),
    onSuccess: (_data, variables) => {
      invalidateCredentialCaches(queryClient, variables.id)
    },
  })
}

// 重置失败计数
export function useResetFailure() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => resetCredentialFailure(id),
    onSuccess: (_data, id) => {
      invalidateCredentialCaches(queryClient, id)
    },
  })
}

// 强制刷新 Token
export function useForceRefreshToken() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => forceRefreshToken(id),
    onSuccess: (_data, id) => {
      invalidateCredentialCaches(queryClient, id)
    },
  })
}

// 添加新凭据
export function useAddCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: AddCredentialRequest) => addCredential(req),
    onSuccess: () => {
      invalidateCredentialCaches(queryClient)
    },
  })
}

// 删除凭据
export function useDeleteCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteCredential(id),
    onSuccess: (_data, id) => {
      invalidateCredentialCaches(queryClient, id)
    },
  })
}

// 获取负载均衡模式
export function useLoadBalancingMode() {
  return useQuery({
    queryKey: ['loadBalancingMode'],
    queryFn: getLoadBalancingMode,
  })
}

// 设置负载均衡模式
export function useSetLoadBalancingMode() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: setLoadBalancingMode,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['loadBalancingMode'] })
    },
  })
}

export function useRuntimeConfig() {
  return useQuery({
    queryKey: ['runtimeConfig'],
    queryFn: getRuntimeConfig,
  })
}

export function useUpdateRuntimeConfig() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: UpdateRuntimeConfigRequest) => updateRuntimeConfig(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['runtimeConfig'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
    },
  })
}
