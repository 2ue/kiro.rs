import { api } from '@/api/http'
import type {
  InventoryActionResponse,
  InventoryImportRequest,
  InventoryImportResponse,
  InventoryItem,
  InventoryListQuery,
  InventoryListResponse,
  InventoryPushRequest,
  InventoryUpdateRequest,
} from '@/types/api'

export async function getInventory(query: InventoryListQuery): Promise<InventoryListResponse> {
  const { data } = await api.get<InventoryListResponse>('/inventory', { params: query })
  return data
}

export async function importInventory(payload: InventoryImportRequest): Promise<InventoryImportResponse> {
  const { data } = await api.post<InventoryImportResponse>('/inventory/import', payload, { timeout: 600_000 })
  return data
}

export async function checkInventory(ids: number[]): Promise<InventoryActionResponse> {
  const { data } = await api.post<InventoryActionResponse>('/inventory/check', { ids }, { timeout: 600_000 })
  return data
}

export async function pushInventory(payload: InventoryPushRequest): Promise<InventoryActionResponse> {
  const { data } = await api.post<InventoryActionResponse>('/inventory/push', payload, { timeout: 600_000 })
  return data
}

export async function deleteInventory(ids: number[]): Promise<InventoryActionResponse> {
  const { data } = await api.post<InventoryActionResponse>('/inventory/delete', { ids })
  return data
}

export async function updateInventory(id: number, payload: InventoryUpdateRequest): Promise<InventoryItem> {
  const { data } = await api.put<InventoryItem>(`/inventory/${id}`, payload)
  return data
}
