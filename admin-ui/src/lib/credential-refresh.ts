import type {
  CredentialInfoRefreshItem,
  CredentialInfoRefreshResponse,
} from '@/types/api'

export type CredentialRefreshSource = 'local_account' | 'external_pool' | 'unknown'

export interface CredentialRefreshFailureGroup {
  key: string
  source: CredentialRefreshSource
  fingerprint: string
  message: string
  count: number
  items: CredentialInfoRefreshItem[]
}

export interface CredentialRefreshReport {
  total: number
  success: number
  failed: number
  groups: CredentialRefreshFailureGroup[]
}

export const CREDIT_INFO_REFRESH_REQUEST_BATCH_SIZE = 20

type RefreshCredentials = (ids: number[]) => Promise<CredentialInfoRefreshResponse>

interface RefreshInBatchesOptions {
  batchSize?: number
  onBatchCompleted?: (
    ids: number[],
    response: CredentialInfoRefreshResponse,
  ) => void
  errorMessage?: (error: unknown) => string
}

const CLIENT_BATCH_FAILURE_PREFIX = '[client-refresh-batch]'
const EXTERNAL_POOL_PATTERN = /(?:\bexternal[\s_-]*(?:upstream[\s_-]*)?pool\b|\bexternal upstream\b|外部(?:备用)?池)/i
const REQUEST_ID_PATTERN = /\s*\(?\b(?:request|trace)[\s_-]*id\s*[:=]\s*[^)\s,;]+[^)]*\)?/gi
const UUID_PATTERN = /\b[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b/gi
const LONG_HASH_PATTERN = /\b[a-f0-9]{16,}\b/gi
const ISO_TIMESTAMP_PATTERN = /\b\d{4}-\d{2}-\d{2}[t\s]\d{2}:\d{2}:\d{2}(?:\.\d+)?z?\b/gi

export function credentialRefreshSourceLabel(source: CredentialRefreshSource): string {
  if (source === 'external_pool') return '外部池'
  if (source === 'local_account') return '本地账号'
  return '未知来源'
}

function classifySource(error: string): CredentialRefreshSource {
  if (error.startsWith(CLIENT_BATCH_FAILURE_PREFIX)) return 'unknown'
  return EXTERNAL_POOL_PATTERN.test(error) ? 'external_pool' : 'local_account'
}

function normalizedFallbackFingerprint(error: string): string {
  const code = error.match(/\b[A-Z][A-Z0-9]+(?:_[A-Z0-9]+)+\b/)
  if (code) return code[0]

  const normalized = error
    .replace(REQUEST_ID_PATTERN, '')
    .replace(UUID_PATTERN, '<uuid>')
    .replace(LONG_HASH_PATTERN, '<hash>')
    .replace(ISO_TIMESTAMP_PATTERN, '<time>')
    .replace(/\b\d+\b/g, '#')
    .replace(/\s+/g, ' ')
    .trim()

  return normalized ? normalized.slice(0, 180) : 'UNKNOWN_CREDENTIAL_REFRESH_ERROR'
}

function fingerprintFor(error: string): string {
  const upper = error.toUpperCase()
  if (upper.includes('THINKING_SIGNATURE_INVALID')) return 'THINKING_SIGNATURE_INVALID'
  if (upper.includes('THINKING_SIGNATURE_RETRY_SEND_LIMIT_EXHAUSTED')) {
    return 'THINKING_SIGNATURE_RETRY_SEND_LIMIT_EXHAUSTED'
  }
  if (upper.includes('TEXT CONTENT BLOCKS MUST CONTAIN NON-WHITESPACE TEXT')) {
    return 'EXTERNAL_NON_WHITESPACE_TEXT_BLOCK'
  }
  return normalizedFallbackFingerprint(error)
}

function compactMessage(error: string): string {
  const normalized = error.replace(/\s+/g, ' ').trim()
  if (!normalized) return '后端未返回错误详情'
  return normalized.length > 220 ? `${normalized.slice(0, 217)}...` : normalized
}

function clientBatchFailureResponse(
  ids: number[],
  error: unknown,
  errorMessage: (error: unknown) => string,
): CredentialInfoRefreshResponse {
  const message = compactMessage(errorMessage(error))
  const itemError = `${CLIENT_BATCH_FAILURE_PREFIX} 客户端请求批次失败: ${message}`

  return {
    total: ids.length,
    success: 0,
    failed: ids.length,
    items: ids.map((id) => ({
      id,
      disabled: false,
      ok: false,
      info: null,
      error: itemError,
    })),
  }
}

export async function refreshCredentialInfoInBatches(
  ids: number[],
  refresh: RefreshCredentials,
  options: RefreshInBatchesOptions = {},
): Promise<CredentialInfoRefreshResponse[]> {
  const batchSize = Math.max(1, Math.floor(options.batchSize ?? CREDIT_INFO_REFRESH_REQUEST_BATCH_SIZE))
  const errorMessage = options.errorMessage ?? (() => '网络或服务请求失败')
  const responses: CredentialInfoRefreshResponse[] = []

  for (let start = 0; start < ids.length; start += batchSize) {
    const batch = ids.slice(start, start + batchSize)
    let response: CredentialInfoRefreshResponse
    try {
      response = await refresh(batch)
    } catch (error) {
      response = clientBatchFailureResponse(batch, error, errorMessage)
    }
    responses.push(response)
    options.onBatchCompleted?.(batch, response)
  }

  return responses
}

export function buildCredentialRefreshReport(
  responses: CredentialInfoRefreshResponse[],
): CredentialRefreshReport {
  const total = responses.reduce((sum, response) => sum + response.total, 0)
  const success = responses.reduce((sum, response) => sum + response.success, 0)
  const reportedFailed = responses.reduce((sum, response) => sum + response.failed, 0)
  const failedItems = responses.flatMap((response) => response.items).filter((item) => !item.ok)
  const failed = Math.max(reportedFailed, failedItems.length)
  const groups = new Map<string, CredentialRefreshFailureGroup>()

  for (const item of failedItems) {
    const error = item.error?.trim() || '后端未返回错误详情'
    const source = classifySource(error)
    const fingerprint = fingerprintFor(error)
    const key = `${source}:${fingerprint}`
    const existing = groups.get(key)
    if (existing) {
      existing.count += 1
      existing.items.push(item)
      continue
    }
    groups.set(key, {
      key,
      source,
      fingerprint,
      message: compactMessage(error),
      count: 1,
      items: [item],
    })
  }

  const missingDetailCount = Math.max(0, failed - failedItems.length)
  if (missingDetailCount > 0) {
    groups.set('unknown:MISSING_FAILURE_DETAILS', {
      key: 'unknown:MISSING_FAILURE_DETAILS',
      source: 'unknown',
      fingerprint: 'MISSING_FAILURE_DETAILS',
      message: '后端汇总了失败数量，但未返回对应账号和错误详情。',
      count: missingDetailCount,
      items: [],
    })
  }

  return {
    total,
    success,
    failed,
    groups: Array.from(groups.values()).sort(
      (left, right) => right.count - left.count || left.key.localeCompare(right.key),
    ),
  }
}
