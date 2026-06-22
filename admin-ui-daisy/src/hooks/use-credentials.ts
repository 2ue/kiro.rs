import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  addCredential,
  batchUpdateCredentials,
  clearCredentialInFlight,
  createProxyResource,
  deleteCredential,
  deleteDisabledCredentials,
  deleteProxyResource,
  forceRefreshToken,
  getCredentialAccountInfo,
  getCredentialBalance,
  getCredentialCreditSummary,
  getCredentialList,
  getCredentialRuntime,
  getCredentialSummary,
  getCredentialUsageSummary,
  getCredentials,
  getCredentialsPage,
  getLoadBalancingMode,
  getProxyResources,
  getRuntimeConfig,
  resetCredentialFailure,
  setCredentialDisabled,
  setCredentialConcurrency,
  setCredentialPriority,
  setCredentialProxy,
  setCredentialRegions,
  setCredentialWarmup,
  setLoadBalancingMode,
  testCredential,
  testProxyResource,
  testProxyResourceConfig,
  updateProxyResource,
  updateRuntimeConfig,
} from '@/api/credentials'
import type {
  AddCredentialRequest,
  BatchUpdateCredentialsRequest,
  CreateProxyResourceRequest,
  CredentialsPageQuery,
  SetCredentialConcurrencyRequest,
  SetCredentialProxyRequest,
  SetCredentialRegionsRequest,
  TestCredentialRequest,
  ProxyResourceTestRequest,
  UpdateProxyResourceRequest,
  UpdateRuntimeConfigRequest,
} from '@/types/api'

function invalidateCredentialCaches(queryClient: ReturnType<typeof useQueryClient>, id?: number) {
  queryClient.invalidateQueries({ queryKey: ['credentials'] })
  queryClient.invalidateQueries({ queryKey: ['credential-list'] })
  queryClient.invalidateQueries({ queryKey: ['credential-summary'] })
  queryClient.invalidateQueries({ queryKey: ['credential-runtime'] })
  queryClient.invalidateQueries({ queryKey: ['credential-account-info'] })
  queryClient.invalidateQueries({ queryKey: ['credential-usage-summary'] })
  queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
  queryClient.invalidateQueries({ queryKey: ['credential-credit-summary'] })
  if (typeof id === 'number') queryClient.invalidateQueries({ queryKey: ['credential-balance', id] })
  else queryClient.invalidateQueries({ queryKey: ['credential-balance'] })
}

export function useCredentials(options: { enabled?: boolean; refetchInterval?: number | false } = {}) {
  return useQuery({
    queryKey: ['credentials'],
    queryFn: getCredentials,
    enabled: options.enabled ?? true,
    refetchInterval: options.refetchInterval ?? 30000,
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

export function useCredentialList(query: CredentialsPageQuery) {
  return useQuery({
    queryKey: ['credential-list', query],
    queryFn: () => getCredentialList(query),
    refetchInterval: 30000,
    placeholderData: (previousData) => previousData,
  })
}

export function useCredentialSummary() {
  return useQuery({
    queryKey: ['credential-summary'],
    queryFn: getCredentialSummary,
    refetchInterval: 5000,
  })
}

export function useCredentialRuntime(ids: number[]) {
  return useQuery({
    queryKey: ['credential-runtime', ids],
    queryFn: () => getCredentialRuntime(ids),
    enabled: ids.length > 0,
    refetchInterval: 5000,
    placeholderData: (previousData) => previousData,
  })
}

export function useCredentialAccountInfo(ids: number[], options: { enabled?: boolean; refetchInterval?: number | false } = {}) {
  return useQuery({
    queryKey: ['credential-account-info', ids],
    queryFn: () => getCredentialAccountInfo(ids),
    enabled: (options.enabled ?? true) && ids.length > 0,
    refetchInterval: options.refetchInterval ?? 60000,
    placeholderData: (previousData) => previousData,
  })
}

export function useCredentialUsageSummary(ids: number[], options: { enabled?: boolean; refetchInterval?: number | false } = {}) {
  return useQuery({
    queryKey: ['credential-usage-summary', ids],
    queryFn: () => getCredentialUsageSummary(ids),
    enabled: (options.enabled ?? true) && ids.length > 0,
    refetchInterval: options.refetchInterval ?? 30000,
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

export function useCredentialCreditSummary() {
  return useQuery({
    queryKey: ['credential-credit-summary'],
    queryFn: getCredentialCreditSummary,
    refetchInterval: 30000,
  })
}

export function useTestCredential() {
  return useMutation({
    mutationFn: ({ id, request }: { id: number; request: TestCredentialRequest }) => testCredential(id, request),
  })
}

export function useProxyResources() {
  return useQuery({
    queryKey: ['proxy-resources'],
    queryFn: getProxyResources,
    refetchInterval: 30000,
  })
}

export function useTestProxyResource() {
  return useMutation({
    mutationFn: ({ id, request }: { id?: number; request: ProxyResourceTestRequest }) =>
      typeof id === 'number'
        ? testProxyResource(id, request)
        : testProxyResourceConfig(request),
  })
}

export function useCreateProxyResource() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: CreateProxyResourceRequest) => createProxyResource(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxy-resources'] })
      invalidateCredentialCaches(queryClient)
    },
  })
}

export function useUpdateProxyResource() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, request }: { id: number; request: UpdateProxyResourceRequest }) => updateProxyResource(id, request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxy-resources'] })
      invalidateCredentialCaches(queryClient)
    },
  })
}

export function useDeleteProxyResource() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteProxyResource(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxy-resources'] })
      invalidateCredentialCaches(queryClient)
    },
  })
}

export function useSetCredentialProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, request }: { id: number; request: SetCredentialProxyRequest }) => setCredentialProxy(id, request),
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['proxy-resources'] })
      invalidateCredentialCaches(queryClient, variables.id)
    },
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

export function useSetCredentialConcurrency() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, request }: { id: number; request: SetCredentialConcurrencyRequest }) => setCredentialConcurrency(id, request),
    onSuccess: (_data, variables) => invalidateCredentialCaches(queryClient, variables.id),
  })
}

export function useSetCredentialRegions() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, request }: { id: number; request: SetCredentialRegionsRequest }) => setCredentialRegions(id, request),
    onSuccess: (_data, variables) => invalidateCredentialCaches(queryClient, variables.id),
  })
}

export function useBatchUpdateCredentials() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: BatchUpdateCredentialsRequest) => batchUpdateCredentials(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxy-resources'] })
      invalidateCredentialCaches(queryClient)
    },
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

export function useDeleteDisabledCredentials() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: deleteDisabledCredentials,
    onSuccess: () => invalidateCredentialCaches(queryClient),
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
