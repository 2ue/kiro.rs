import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  SuccessResponse,
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

export async function getUsageSummary(): Promise<UsageSummary> {
  const { data } = await api.get<UsageSummary>('/usage-summary')
  return data
}

export async function clearUsageRecords(): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>('/usage-records/clear')
  return data
}
