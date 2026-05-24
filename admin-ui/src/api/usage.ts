import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  AdminAuditLogPage,
  AdminAuditLogPageQuery,
  SuccessResponse,
  ModelCapabilitiesStatus,
  ModelPricingStatus,
  UsageRecordsPageQuery,
  UsageRecordsPageResult,
  UsageRecordsQuery,
  UsageRecordsResult,
  UsageSummary,
} from '@/types/api'

const api = axios.create({
  baseURL: '/api/admin',
  headers: {
    'Content-Type': 'application/json',
  },
})

api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})

export async function getUsageRecords(
  query: UsageRecordsQuery = {}
): Promise<UsageRecordsResult> {
  const { data } = await api.get<UsageRecordsResult>('/usage-records', {
    params: query,
  })
  return data
}

export async function getUsageRecordsPage(
  query: UsageRecordsPageQuery
): Promise<UsageRecordsPageResult> {
  const { data } = await api.get<UsageRecordsPageResult>('/usage-records-paged', {
    params: query,
  })
  return data
}

export async function getUsageSummary(): Promise<UsageSummary> {
  const { data } = await api.get<UsageSummary>('/usage-summary')
  return data
}

export async function clearUsageRecords(): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>('/usage-records/clear')
  return data
}

export async function getAuditLogsPage(
  query: AdminAuditLogPageQuery
): Promise<AdminAuditLogPage> {
  const { data } = await api.get<AdminAuditLogPage>('/audit-logs', {
    params: query,
  })
  return data
}

export async function getModelPricing(): Promise<ModelPricingStatus> {
  const { data } = await api.get<ModelPricingStatus>('/model-pricing')
  return data
}

export async function syncModelPricing(): Promise<ModelPricingStatus> {
  const { data } = await api.post<ModelPricingStatus>('/model-pricing/sync')
  return data
}

export async function getModelCapabilities(): Promise<ModelCapabilitiesStatus> {
  const { data } = await api.get<ModelCapabilitiesStatus>('/model-capabilities')
  return data
}

export async function syncModelCapabilities(): Promise<ModelCapabilitiesStatus> {
  const { data } = await api.post<ModelCapabilitiesStatus>('/model-capabilities/sync')
  return data
}
