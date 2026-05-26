import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  addCredential,
  clearCredentialInFlight,
  deleteCredential,
  forceRefreshToken,
  getCredentialBalance,
  getCredentials,
  getCredentialsPage,
  getLoadBalancingMode,
  getRuntimeConfig,
  resetCredentialFailure,
  setCredentialDisabled,
  setCredentialPriority,
  setCredentialWarmup,
  setLoadBalancingMode,
  testCredential,
  updateRuntimeConfig,
} from '@/api/credentials'
import type {
  AddCredentialRequest,
  CredentialsPageQuery,
  TestCredentialRequest,
  UpdateRuntimeConfigRequest,
} from '@/types/api'

function invalidateCredentialCaches(queryClient: ReturnType<typeof useQueryClient>, id?: number) {
  queryClient.invalidateQueries({ queryKey: ['credentials'] })
  queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
  if (typeof id === 'number') queryClient.invalidateQueries({ queryKey: ['credential-balance', id] })
  else queryClient.invalidateQueries({ queryKey: ['credential-balance'] })
}

export function useCredentials(options: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: ['credentials'],
    queryFn: getCredentials,
    enabled: options.enabled ?? true,
    refetchInterval: 30000,
  })
}

export function useCredentialsPage(query: CredentialsPageQuery) {
  return useQuery({
    queryKey: ['credentials-page', query],
    queryFn: () => getCredentialsPage(query),
    refetchInterval: 30000,
    placeholderData: (previousData) => previousData,
  })
}

export function useCredentialBalance(id: number | null) {
  return useQuery({
    queryKey: ['credential-balance', id],
    queryFn: () => getCredentialBalance(id!),
    enabled: id !== null,
    retry: false,
  })
}

export function useTestCredential() {
  return useMutation({
    mutationFn: ({ id, request }: { id: number; request: TestCredentialRequest }) => testCredential(id, request),
  })
}

export function useSetDisabled() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, disabled }: { id: number; disabled: boolean }) => setCredentialDisabled(id, disabled),
    onSuccess: (_data, variables) => invalidateCredentialCaches(queryClient, variables.id),
  })
}

export function useSetPriority() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, priority }: { id: number; priority: number }) => setCredentialPriority(id, priority),
    onSuccess: (_data, variables) => invalidateCredentialCaches(queryClient, variables.id),
  })
}

export function useSetWarmup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, warmupRemaining }: { id: number; warmupRemaining: number }) => setCredentialWarmup(id, warmupRemaining),
    onSuccess: (_data, variables) => invalidateCredentialCaches(queryClient, variables.id),
  })
}

export function useClearInFlight() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, minIdleSecs }: { id: number; minIdleSecs?: number }) => clearCredentialInFlight(id, minIdleSecs),
    onSuccess: (_data, variables) => invalidateCredentialCaches(queryClient, variables.id),
  })
}

export function useResetFailure() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => resetCredentialFailure(id),
    onSuccess: (_data, id) => invalidateCredentialCaches(queryClient, id),
  })
}

export function useForceRefreshToken() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => forceRefreshToken(id),
    onSuccess: (_data, id) => invalidateCredentialCaches(queryClient, id),
  })
}

export function useAddCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: AddCredentialRequest) => addCredential(req),
    onSuccess: () => invalidateCredentialCaches(queryClient),
  })
}

export function useDeleteCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteCredential(id),
    onSuccess: (_data, id) => invalidateCredentialCaches(queryClient, id),
  })
}

export function useLoadBalancingMode() {
  return useQuery({
    queryKey: ['load-balancing-mode'],
    queryFn: getLoadBalancingMode,
  })
}

export function useSetLoadBalancingMode() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: setLoadBalancingMode,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['load-balancing-mode'] }),
  })
}

export function useRuntimeConfig() {
  return useQuery({
    queryKey: ['runtime-config'],
    queryFn: getRuntimeConfig,
  })
}

export function useUpdateRuntimeConfig() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: UpdateRuntimeConfigRequest) => updateRuntimeConfig(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['runtime-config'] })
      invalidateCredentialCaches(queryClient)
    },
  })
}
