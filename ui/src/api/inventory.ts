import { api } from '@/api/http'
import type {
  InventoryActionResponse,
  InventoryExportFormat,
  InventoryExportScope,
  InventoryGroupChangeResponse,
  InventoryGroupsResponse,
  InventoryIdsResponse,
  InventoryImportRequest,
  InventoryImportResponse,
  InventoryItem,
  InventoryListQuery,
  InventoryListResponse,
  InventoryProxyRequest,
  InventoryPushRequest,
  InventorySortQuery,
  InventoryTestRequest,
  InventoryTestResponse,
  InventoryFilter,
  InventoryUpdateRequest,
  MoveToInventoryPreview,
  MoveToInventoryRequest,
  MoveToInventoryResponse,
} from '@/types/api'

export async function getInventory(query: InventoryListQuery): Promise<InventoryListResponse> {
  const { data } = await api.get<InventoryListResponse>('/inventory', { params: query })
  return data
}

/** 当前筛选下所有在库、失效账号的 ID，用于跨页全选。 */
export async function getInventoryIds(query: InventoryFilter & InventorySortQuery): Promise<InventoryIdsResponse> {
  const { data } = await api.get<InventoryIdsResponse>('/inventory/ids', { params: query })
  return data
}

/** 用库存账号真实调一次模型；Token 过期会先刷新并写回库存。 */
export async function testInventory(id: number, payload: InventoryTestRequest): Promise<InventoryTestResponse> {
  const { data } = await api.post<InventoryTestResponse>(`/inventory/${id}/test`, payload, { timeout: 180_000 })
  return data
}

export async function setInventoryProxy(payload: InventoryProxyRequest): Promise<InventoryActionResponse> {
  const { data } = await api.post<InventoryActionResponse>('/inventory/proxy', payload)
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

export async function getInventoryGroups(): Promise<InventoryGroupsResponse> {
  const { data } = await api.get<InventoryGroupsResponse>('/inventory/groups')
  return data
}

/** 设置分组；group 为空清除分组。 */
export async function setInventoryGroup(ids: number[], group: string | null): Promise<InventoryGroupChangeResponse> {
  const { data } = await api.post<InventoryGroupChangeResponse>('/inventory/group', { ids, group })
  return data
}

export async function renameInventoryGroup(name: string, newName: string): Promise<InventoryGroupChangeResponse> {
  const { data } = await api.post<InventoryGroupChangeResponse>('/inventory/groups/rename', { name, newName })
  return data
}

/** 解散分组：账号保留，变成未分组。 */
export async function deleteInventoryGroup(name: string): Promise<InventoryGroupChangeResponse> {
  const { data } = await api.post<InventoryGroupChangeResponse>('/inventory/groups/delete', { name })
  return data
}

/** 导出库存账号。`count` 是写入文件的账号数，`skipped` 是已入池（不含密钥）而跳过的数量。 */
export async function exportInventory(
  format: InventoryExportFormat,
  scope: InventoryExportScope,
): Promise<{ blob: Blob; count: number | null; skipped: number }> {
  const params = 'ids' in scope
    ? { format, ids: scope.ids.join(',') }
    : { format, ...scope.filter }
  try {
    const { data, headers } = await api.get<Blob>('/inventory/export', { params, responseType: 'blob' })
    const count = Number(headers['x-inventory-count'])
    const skipped = Number(headers['x-inventory-skipped'])
    return { blob: data, count: Number.isFinite(count) ? count : null, skipped: Number.isFinite(skipped) ? skipped : 0 }
  } catch (error) {
    // With responseType 'blob' the JSON error body arrives as a Blob; decode it so the
    // backend's message reaches the toast.
    const response = (error as { response?: { data?: unknown } })?.response
    if (response?.data instanceof Blob) {
      try {
        response.data = JSON.parse(await response.data.text())
      } catch {
        // Not JSON: keep the generic axios message.
      }
    }
    throw error
  }
}

export async function previewMoveToInventory(payload: MoveToInventoryRequest): Promise<MoveToInventoryPreview> {
  const { data } = await api.post<MoveToInventoryPreview>('/credentials/move-to-inventory/preview', payload)
  return data
}

export async function moveToInventory(payload: MoveToInventoryRequest): Promise<MoveToInventoryResponse> {
  const { data } = await api.post<MoveToInventoryResponse>('/credentials/move-to-inventory', payload, { timeout: 600_000 })
  return data
}
