import axios from 'axios'
import { storage } from '@/lib/storage'

export const api = axios.create({
  baseURL: '/api/admin',
  headers: {
    'Content-Type': 'application/json',
  },
})

function isAdminAuthFailure(status?: number) {
  return status === 401 || status === 403
}

api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) config.headers['x-api-key'] = apiKey
  return config
})

api.interceptors.response.use(
  (response) => response,
  (error) => {
    if (isAdminAuthFailure(error?.response?.status) && typeof window !== 'undefined') {
      window.dispatchEvent(new CustomEvent('kiro-admin-auth-failed'))
    }
    return Promise.reject(error)
  }
)

export async function validateAdminApiKey(apiKey: string): Promise<void> {
  await axios.get('/api/admin/config/load-balancing', {
    headers: {
      'x-api-key': apiKey,
    },
  })
}
