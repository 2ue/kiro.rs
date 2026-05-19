import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { listPricing, syncPricing } from '@/api/admin'

export function usePricing() {
  return useQuery({
    queryKey: ['pricing'],
    queryFn: listPricing,
    staleTime: 60_000,
  })
}

export function useSyncPricing() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (forceBuiltin?: boolean) => syncPricing(forceBuiltin ?? false),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['pricing'] }),
  })
}
