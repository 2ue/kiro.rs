import { http } from '@/api/client'
import type {
  CreateExternalPoolRequest,
  DiscoverExternalPoolSupportedModelsRequest,
  ExternalPool,
  ExternalPoolTestRequest,
  ExternalPoolTestResponse,
  ExternalPoolsListResponse,
  ExternalPoolsStatusResponse,
  SuccessResponse,
  SupportedModelsResponse,
  UpdateExternalPoolRequest,
} from '@/api/types'

export const poolsApi = {
  list: () => http.get<ExternalPoolsListResponse>('/external-pools'),
  status: () => http.get<ExternalPoolsStatusResponse>('/external-pools/status'),
  create: (req: CreateExternalPoolRequest) => http.post<ExternalPool>('/external-pools', req),
  update: (id: number, req: UpdateExternalPoolRequest) => http.put<ExternalPool>(`/external-pools/${id}`, req),
  remove: (id: number) => http.delete<SuccessResponse>(`/external-pools/${id}`),
  setEnabled: (id: number, enabled: boolean) => http.post<ExternalPool>(`/external-pools/${id}/enabled`, { enabled }),
  clearAutoDisabled: (id: number) => http.post<ExternalPool>(`/external-pools/${id}/auto-disabled/clear`),
  clearCooldown: (id: number) => http.post<ExternalPool>(`/external-pools/${id}/cooldown/clear`),
  setSupportedModels: (id: number, supportedModels: string[]) =>
    http.post<SupportedModelsResponse>(`/external-pools/${id}/supported-models`, { supportedModels }),
  syncSupportedModels: (id: number, req: DiscoverExternalPoolSupportedModelsRequest = {}) =>
    http.post<SupportedModelsResponse>(`/external-pools/${id}/supported-models/sync`, req),
  discoverStored: (id: number, req: DiscoverExternalPoolSupportedModelsRequest = {}) =>
    http.post<SupportedModelsResponse>(`/external-pools/${id}/supported-models/discover`, req),
  discover: (req: DiscoverExternalPoolSupportedModelsRequest) =>
    http.post<SupportedModelsResponse>('/external-pools/supported-models/discover', req),
  test: (id: number, req: ExternalPoolTestRequest) => http.post<ExternalPoolTestResponse>(`/external-pools/${id}/test`, req),
}
