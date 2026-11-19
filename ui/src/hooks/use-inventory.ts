import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import {
  checkInventory,
  deleteInventory,
  deleteInventoryGroup,
  getInventory,
  getInventoryGroups,
  importInventory,
  moveToInventory,
  previewMoveToInventory,
  pushInventory,
  renameInventoryGroup,
  setInventoryGroup,
  setInventoryProxy,
  testInventory,
  updateInventory,
} from '@/api/inventory'
import type {
  InventoryListQuery,
  InventoryProxyRequest,
  InventoryPushRequest,
  InventoryTestRequest,
  InventoryUpdateRequest,
  MoveToInventoryRequest,
} from '@/types/api'

function invalidateInventory(queryClient: QueryClient, poolChanged = false) {
  queryClient.invalidateQueries({ queryKey: ['inventory'] })
  queryClient.invalidateQueries({ queryKey: ['inventory-groups'] })
  if (poolChanged) {
    queryClient.invalidateQueries({ queryKey: ['credential-list'] })
    queryClient.invalidateQueries({ queryKey: ['credential-summary'] })
    queryClient.invalidateQueries({ queryKey: ['credential-facets'] })
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

export function useTestInventory() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, payload }: { id: number; payload: InventoryTestRequest }) => testInventory(id, payload),
    onSuccess: () => invalidateInventory(queryClient),
  })
}

export function useSetInventoryProxy() {
  const queryClient = useQueryClient()
  return useMutation({ mutationFn: (payload: InventoryProxyRequest) => setInventoryProxy(payload), onSuccess: () => invalidateInventory(queryClient) })
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

export function useInventoryGroups(enabled = true) {
  return useQuery({
    queryKey: ['inventory-groups'],
    queryFn: getInventoryGroups,
    enabled,
    staleTime: 10_000,
  })
}

export function useSetInventoryGroup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ ids, group }: { ids: number[]; group: string | null }) => setInventoryGroup(ids, group),
    onSuccess: () => invalidateInventory(queryClient),
  })
}

export function useRenameInventoryGroup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ name, newName }: { name: string; newName: string }) => renameInventoryGroup(name, newName),
    onSuccess: () => invalidateInventory(queryClient),
  })
}

export function useDeleteInventoryGroup() {
  const queryClient = useQueryClient()
  return useMutation({ mutationFn: (name: string) => deleteInventoryGroup(name), onSuccess: () => invalidateInventory(queryClient) })
}

/** 禁用账号入库的预览；选项变化时重新计算。 */
export function useMoveToInventoryPreview(payload: MoveToInventoryRequest, enabled: boolean) {
  return useQuery({
    queryKey: ['move-to-inventory-preview', payload.ids ?? null, payload.excludeAbnormal, payload.reasons ?? null],
    queryFn: () => previewMoveToInventory(payload),
    enabled,
    staleTime: 0,
    placeholderData: (previous) => previous,
  })
}

export function useMoveToInventory() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (payload: MoveToInventoryRequest) => moveToInventory(payload),
    onSuccess: () => {
      invalidateInventory(queryClient, true)
      queryClient.invalidateQueries({ queryKey: ['move-to-inventory-preview'] })
    },
  })
}
