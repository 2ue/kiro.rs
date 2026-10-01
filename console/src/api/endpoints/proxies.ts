import { http } from '@/api/client'
import type {
  BatchProxyResourceImportRequest,
  BatchProxyResourceImportResponse,
  CreateProxyResourceRequest,
  ProxyResource,
  ProxyResourceTestRequest,
  ProxyResourceTestResponse,
  ProxyResourcesResponse,
  SuccessResponse,
  UpdateProxyResourceRequest,
} from '@/api/types'

export const proxiesApi = {
  list: () => http.get<ProxyResourcesResponse>('/proxy-resources'),
  create: (req: CreateProxyResourceRequest) => http.post<ProxyResource>('/proxy-resources', req),
  update: (id: number, req: UpdateProxyResourceRequest) => http.put<ProxyResource>(`/proxy-resources/${id}`, req),
  remove: (id: number) => http.delete<SuccessResponse>(`/proxy-resources/${id}`),
  import: (req: BatchProxyResourceImportRequest) => http.post<BatchProxyResourceImportResponse>('/proxy-resources/import', req),
  test: (id: number, req: ProxyResourceTestRequest = {}) => http.post<ProxyResourceTestResponse>(`/proxy-resources/${id}/test`, req),
  testConfig: (req: ProxyResourceTestRequest) => http.post<ProxyResourceTestResponse>('/proxy-resources/test', req),
}
