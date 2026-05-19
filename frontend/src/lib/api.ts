import axios, { type AxiosInstance } from 'axios'
import { storage } from '@/lib/storage'

export const adminApi: AxiosInstance = axios.create({
  baseURL: '/api/admin',
  headers: { 'Content-Type': 'application/json' },
})

adminApi.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})
