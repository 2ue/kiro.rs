import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  addCredential,
  deleteCredential,
  forceRefreshToken,
  getCredentialBalance,
  getCredentials,
  getCredentialsPage,
  getLoadBalancingMode,
  resetCredentialFailure,
  setCredentialDisabled,
  setCredentialPriority,
  setLoadBalancingMode,
} from '@/api/admin'
import type {
  AddCredentialRequest,
  CredentialsPageQuery,
  LoadBalancingMode,
} from '@/types/api'

function invalidateAll(qc: ReturnType<typeof useQueryClient>) {
  qc.invalidateQueries({ queryKey: ['credentials'] })
  qc.invalidateQueries({ queryKey: ['credentials-page'] })
  qc.invalidateQueries({ queryKey: ['credential-balance'] })
}

export function useCredentialsList(enabled = true) {
  return useQuery({
    queryKey: ['credentials'],
    queryFn: getCredentials,
    enabled,
    refetchInterval: 30_000,
  })
}

export function useCredentialsPage(query: CredentialsPageQuery) {
  return useQuery({
    queryKey: ['credentials-page', query],
    queryFn: () => getCredentialsPage(query),
    refetchInterval: 30_000,
    placeholderData: (prev) => prev,
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

export function useSetDisabled() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, disabled }: { id: number; disabled: boolean }) =>
      setCredentialDisabled(id, disabled),
    onSuccess: () => invalidateAll(qc),
  })
}

export function useSetPriority() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, priority }: { id: number; priority: number }) =>
      setCredentialPriority(id, priority),
    onSuccess: () => invalidateAll(qc),
  })
}

export function useResetFailure() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => resetCredentialFailure(id),
    onSuccess: () => invalidateAll(qc),
  })
}

export function useForceRefreshToken() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => forceRefreshToken(id),
    onSuccess: () => invalidateAll(qc),
  })
}

export function useAddCredential() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (req: AddCredentialRequest) => addCredential(req),
    onSuccess: () => invalidateAll(qc),
  })
}

export function useDeleteCredential() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteCredential(id),
    onSuccess: () => invalidateAll(qc),
  })
}

export function useLoadBalancingMode() {
  return useQuery({ queryKey: ['load-balancing'], queryFn: getLoadBalancingMode })
}

export function useSetLoadBalancingMode() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (mode: LoadBalancingMode) => setLoadBalancingMode(mode),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['load-balancing'] }),
  })
}
