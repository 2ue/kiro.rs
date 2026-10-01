export interface ProxyResource {
  id: number
  name: string
  proxyUrl: string
  proxyUsername?: string | null
  proxyPassword?: string | null
  hasPassword: boolean
  enabled: boolean
  notes?: string | null
  createdAt: string
  updatedAt: string
  credentialCount: number
}

export interface ProxyResourcesResponse {
  resources: ProxyResource[]
}

export interface ProxyResourceTestRequest {
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  testUrl?: string
}

export interface ProxyResourceTestResponse {
  success: boolean
  message: string
  proxyUrl: string
  testUrl: string
  status?: number | null
  durationMs: number
  responsePreview?: string
}

export interface CreateProxyResourceRequest {
  name: string
  proxyUrl: string
  proxyUsername?: string
  proxyPassword?: string
  enabled?: boolean
  notes?: string
}

export interface BatchProxyResourceImportRequest {
  content: string
  namePrefix?: string
  enabled?: boolean
  continueOnError?: boolean
}

export interface BatchProxyResourceImportItem {
  index: number
  ok: boolean
  resourceId?: number
  name?: string
  proxyUrl?: string
  error?: string
}

export interface BatchProxyResourceImportResponse {
  total: number
  success: number
  failed: number
  items: BatchProxyResourceImportItem[]
}

export interface UpdateProxyResourceRequest {
  name?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  clearUsername?: boolean
  clearPassword?: boolean
  enabled?: boolean
  notes?: string
  clearNotes?: boolean
}

export interface SetCredentialProxyRequest {
  proxyResourceId?: number | null
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
}
