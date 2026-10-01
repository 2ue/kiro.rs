import { AUTH_EXPIRED_EVENT, authStorage } from '@/lib/auth-storage'

const BASE = '/api/admin'

/** 统一的管理 API 错误；message 已从后端 `{ error: { type, message } }` 中提取 */
export class ApiError extends Error {
  readonly status: number
  readonly type?: string
  readonly raw?: unknown

  constructor(status: number, message: string, type?: string, raw?: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.type = type
    this.raw = raw
  }
}

type QueryValue = string | number | boolean | null | undefined
export type Query = Record<string, QueryValue | QueryValue[]>

function buildUrl(path: string, query?: Query): string {
  const url = new URL(BASE + path, window.location.origin)
  if (query) {
    for (const [key, value] of Object.entries(query)) {
      if (value === undefined || value === null || value === '') continue
      url.searchParams.set(key, Array.isArray(value) ? value.join(',') : String(value))
    }
  }
  return url.pathname + url.search
}

async function parseError(response: Response): Promise<ApiError> {
  let raw: unknown
  try {
    raw = await response.json()
  } catch {
    raw = undefined
  }
  const err = (raw as { error?: { type?: string; message?: string } } | undefined)?.error
  if (err?.message) return new ApiError(response.status, err.message, err.type, raw)
  if (response.status === 404) {
    return new ApiError(404, '接口不存在：后端版本可能未更新，请确认服务已重启并加载最新版本', undefined, raw)
  }
  return new ApiError(response.status, `请求失败（HTTP ${response.status}）`, undefined, raw)
}

interface RequestOptions {
  query?: Query
  body?: unknown
  apiKey?: string
  signal?: AbortSignal
  responseType?: 'json' | 'blob'
}

async function request<T>(method: string, path: string, options: RequestOptions = {}): Promise<T> {
  const apiKey = options.apiKey ?? authStorage.get()
  const headers: Record<string, string> = {}
  if (apiKey) headers['x-api-key'] = apiKey
  if (options.body !== undefined) headers['Content-Type'] = 'application/json'

  let response: Response
  try {
    response = await fetch(buildUrl(path, options.query), {
      method,
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: options.signal,
    })
  } catch (error) {
    if ((error as Error).name === 'AbortError') throw error
    throw new ApiError(0, '无法连接到服务，请检查网络或服务是否运行')
  }

  if (!response.ok) {
    const error = await parseError(response)
    // 只有使用已存储 Key 的请求失败才视为会话失效；登录校验时的 401 由调用方处理
    if ((response.status === 401 || response.status === 403) && options.apiKey === undefined) {
      window.dispatchEvent(new CustomEvent(AUTH_EXPIRED_EVENT))
    }
    throw error
  }

  if (options.responseType === 'blob') return (await response.blob()) as T
  if (response.status === 204) return undefined as T
  const text = await response.text()
  return (text ? JSON.parse(text) : undefined) as T
}

export const http = {
  get: <T>(path: string, query?: Query, options?: Omit<RequestOptions, 'query' | 'body'>) =>
    request<T>('GET', path, { ...options, query }),
  post: <T>(path: string, body?: unknown, options?: Omit<RequestOptions, 'body'>) =>
    request<T>('POST', path, { ...options, body }),
  put: <T>(path: string, body?: unknown) => request<T>('PUT', path, { body }),
  patch: <T>(path: string, body?: unknown) => request<T>('PATCH', path, { body }),
  delete: <T>(path: string) => request<T>('DELETE', path),
}

export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message
  if (error instanceof Error) return error.message
  return '未知错误'
}
