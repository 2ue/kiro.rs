import { http } from '@/api/client'
import type {
  AddCredentialRequest,
  AddCredentialResponse,
  BalanceResponse,
  BatchCredentialImportRequest,
  BatchCredentialImportResponse,
  BatchUpdateCredentialsRequest,
  BatchUpdateCredentialsResponse,
  BulkCredentialActionResponse,
  CredentialAccountInfoListResponse,
  CredentialCreditSummaryResponse,
  CredentialDiagnosticsResponse,
  CredentialExportFormat,
  CredentialInfoRefreshResponse,
  CredentialListResponse,
  CredentialRuntimeResponse,
  CredentialSummaryResponse,
  CredentialUsageSummaryResponse,
  CredentialValidationResponse,
  CredentialsPageQuery,
  CredentialsPageResponse,
  SetCredentialProxyRequest,
  SetCredentialRegionsRequest,
  SuccessResponse,
  SupportedModelsResponse,
  TestCredentialRequest,
  TestCredentialResponse,
  ValidateExistingCredentialsRequest,
  ValidateExternalCredentialsRequest,
} from '@/api/types'

const ids = (list: number[]) => ({ ids: list.join(',') })

export const credentialsApi = {
  page: (query: CredentialsPageQuery) => http.get<CredentialsPageResponse>('/credentials-paged', { ...query }),
  list: (query: CredentialsPageQuery) => http.get<CredentialListResponse>('/credentials/list', { ...query }),
  summary: () => http.get<CredentialSummaryResponse>('/credentials/summary'),
  runtime: (list: number[]) => http.get<CredentialRuntimeResponse>('/credentials/runtime', ids(list)),
  accountInfo: (list: number[]) => http.get<CredentialAccountInfoListResponse>('/credentials/account-info', ids(list)),
  usageSummary: (list: number[]) => http.get<CredentialUsageSummaryResponse>('/credentials/usage-summary', ids(list)),
  creditSummary: () => http.get<CredentialCreditSummaryResponse>('/credentials/credit-summary'),
  diagnostics: (id: number, page = 1, limit = 20) =>
    http.get<CredentialDiagnosticsResponse>(`/credentials/${id}/diagnostics`, { page, limit }),
  balance: (id: number) => http.get<BalanceResponse>(`/credentials/${id}/balance`),
  info: (id: number, force = true) => http.get<BalanceResponse>(`/credentials/${id}/info`, { force }),
  refreshInfo: (list: number[], force = true) =>
    http.post<CredentialInfoRefreshResponse>('/credentials/info/refresh', { ids: list, force }),

  add: (req: AddCredentialRequest) => http.post<AddCredentialResponse>('/credentials', req),
  import: (req: BatchCredentialImportRequest) => http.post<BatchCredentialImportResponse>('/credentials/import', req),
  batchUpdate: (req: BatchUpdateCredentialsRequest) => http.post<BatchUpdateCredentialsResponse>('/credentials/batch-update', req),
  export: (format: CredentialExportFormat, list?: number[]) =>
    http.get<Blob>('/credentials/export', { format, ids: list?.length ? list.join(',') : undefined }, { responseType: 'blob' }),
  remove: (id: number) => http.delete<SuccessResponse>(`/credentials/${id}`),
  removeDisabled: () => http.delete<BulkCredentialActionResponse>('/credentials/disabled'),

  setDisabled: (id: number, disabled: boolean) => http.post<SuccessResponse>(`/credentials/${id}/disabled`, { disabled }),
  setPriority: (id: number, priority: number) => http.post<SuccessResponse>(`/credentials/${id}/priority`, { priority }),
  setConcurrency: (id: number, maxConcurrentRequests: number | null) =>
    http.post<SuccessResponse>(`/credentials/${id}/concurrency`, { maxConcurrentRequests }),
  setRpm: (id: number, rpm: number | null) => http.post<SuccessResponse>(`/credentials/${id}/rpm`, { rpm }),
  setRateLimitAutoDisable: (id: number, enabled: boolean) =>
    http.post<SuccessResponse>(`/credentials/${id}/rate-limit-auto-disable`, { enabled }),
  setRegions: (id: number, req: SetCredentialRegionsRequest) => http.post<SuccessResponse>(`/credentials/${id}/regions`, req),
  setWarmup: (id: number, warmupRemaining: number) => http.post<SuccessResponse>(`/credentials/${id}/warmup`, { warmupRemaining }),
  setProxy: (id: number, req: SetCredentialProxyRequest) => http.post<SuccessResponse>(`/credentials/${id}/proxy`, req),
  setOverage: (id: number, enabled: boolean) => http.post<BalanceResponse>(`/credentials/${id}/overage`, { enabled }),
  setSupportedModels: (id: number, supportedModels: string[]) =>
    http.post<SupportedModelsResponse>(`/credentials/${id}/supported-models`, { supportedModels }),
  syncSupportedModels: (id: number) => http.post<SupportedModelsResponse>(`/credentials/${id}/supported-models/sync`),
  discoverSupportedModels: (id: number) => http.post<SupportedModelsResponse>(`/credentials/${id}/supported-models/discover`),
  updateAuth: (id: number, req: Partial<AddCredentialRequest> & { resetRuntimeState?: boolean }) =>
    http.patch<SuccessResponse>(`/credentials/${id}/auth`, req),

  clearInFlight: (id: number, minIdleSecs?: number) => http.post<SuccessResponse>(`/credentials/${id}/in-flight/clear`, { minIdleSecs }),
  resetFailures: (id: number) => http.post<SuccessResponse>(`/credentials/${id}/reset`),
  refreshToken: (id: number) => http.post<SuccessResponse>(`/credentials/${id}/refresh`),
  test: (id: number, req: TestCredentialRequest) => http.post<TestCredentialResponse>(`/credentials/${id}/test`, req),

  validateExisting: (req: ValidateExistingCredentialsRequest) =>
    http.post<CredentialValidationResponse>('/credential-validation/existing', req),
  validateExternal: (req: ValidateExternalCredentialsRequest) =>
    http.post<CredentialValidationResponse>('/credential-validation/external', req),
}
