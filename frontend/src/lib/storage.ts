const KEY = 'kiro-rs.adminApiKey'

export const storage = {
  getApiKey(): string | null {
    try {
      return localStorage.getItem(KEY)
    } catch {
      return null
    }
  },
  setApiKey(value: string): void {
    try {
      localStorage.setItem(KEY, value)
    } catch {
      /* ignore */
    }
  },
  removeApiKey(): void {
    try {
      localStorage.removeItem(KEY)
    } catch {
      /* ignore */
    }
  },
}
