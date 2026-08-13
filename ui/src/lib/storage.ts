export const ADMIN_API_KEY_FIELD = 'adminApiKey'
const ADMIN_API_KEY_STORAGE_KEY = ADMIN_API_KEY_FIELD

export function maskAdminApiKey(key?: string | null): string {
  if (!key) return '未保存'
  if (key.length <= 10) return `${key.slice(0, 2)}...${key.slice(-2)}`
  return `${key.slice(0, 6)}...${key.slice(-4)}`
}

export const storage = {
  getApiKey: () => localStorage.getItem(ADMIN_API_KEY_STORAGE_KEY),
  setApiKey: (key: string) => localStorage.setItem(ADMIN_API_KEY_STORAGE_KEY, key),
  removeApiKey: () => localStorage.removeItem(ADMIN_API_KEY_STORAGE_KEY),
}
