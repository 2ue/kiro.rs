const KEY = 'kiro-console.adminApiKey'

/**
 * Admin Key 默认只存在 sessionStorage（关闭标签页即失效）；
 * 用户显式勾选"在此设备记住"时才写入 localStorage。
 */
export const authStorage = {
  get(): string | null {
    return sessionStorage.getItem(KEY) ?? localStorage.getItem(KEY)
  },
  set(value: string, remember: boolean) {
    sessionStorage.setItem(KEY, value)
    if (remember) localStorage.setItem(KEY, value)
    else localStorage.removeItem(KEY)
  },
  isRemembered(): boolean {
    return localStorage.getItem(KEY) !== null
  },
  clear() {
    sessionStorage.removeItem(KEY)
    localStorage.removeItem(KEY)
  },
}

export const AUTH_EXPIRED_EVENT = 'kiro-console:auth-expired'
