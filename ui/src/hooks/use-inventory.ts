import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import {
  checkInventory,
  deleteInventory,
  getInventory,
  importInventory,
  pushInventory,
  updateInventory,
} from '@/api/inventory'
import type { InventoryListQuery, InventoryPushRequest, InventoryUpdateRequest } from '@/types/api'

function invalidateInventory(queryClient: QueryClient, poolChanged = false) {
  queryClient.invalidateQueries({ queryKey: ['inventory'] })
  if (poolChanged) {
    queryClient.invalidateQueries({ queryKey: ['credential-list'] })
    queryClient.invalidateQueries({ queryKey: ['credential-summary'] })
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
  }
}

export function useInventory(query: InventoryListQuery) {
  return useQuery({
    queryKey: ['inventory', query],
    queryFn: () => getInventory(query),
    refetchInterval: 30_000,
    placeholderData: (previous) => previous,
  })
}

export function useImportInventory() {
  const queryClient = useQueryClient()
  return useMutation({ mutationFn: importInventory, onSuccess: () => invalidateInventory(queryClient) })
}

export function useCheckInventory() {
  const queryClient = useQueryClient()
  return useMutation({ mutationFn: (ids: number[]) => checkInventory(ids), onSuccess: () => invalidateInventory(queryClient) })
}

export function usePushInventory() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (payload: InventoryPushRequest) => pushInventory(payload),
    onSuccess: () => invalidateInventory(queryClient, true),
  })
}

export function useDeleteInventory() {
  const queryClient = useQueryClient()
  return useMutation({ mutationFn: (ids: number[]) => deleteInventory(ids), onSuccess: () => invalidateInventory(queryClient) })
}

export function useUpdateInventory() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, payload }: { id: number; payload: InventoryUpdateRequest }) => updateInventory(id, payload),
    onSuccess: () => invalidateInventory(queryClient),
  })
}
