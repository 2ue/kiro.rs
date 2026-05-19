import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { listAppConfig, updateAppConfig } from '@/api/admin'

export function useAppConfig() {
  return useQuery({
    queryKey: ['app-config'],
    queryFn: listAppConfig,
    staleTime: 30_000,
  })
}

export function useUpdateAppConfig() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (items: Record<string, unknown>) => updateAppConfig(items),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['app-config'] }),
  })
}
