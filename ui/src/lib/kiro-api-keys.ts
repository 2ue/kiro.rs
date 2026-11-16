import type { BatchCreateKiroApiKeysResponse, CreateKiroApiKeyItem } from '../types/api.ts'

export function isKiroApiKeyAccount(credential: { authMethod?: string | null; apiKeyHash?: string | null; maskedApiKey?: string | null }): boolean {
  return credential.authMethod?.toLowerCase() === 'api_key' || Boolean(credential.apiKeyHash || credential.maskedApiKey)
}

export type KiroKeyExportFormat = 'txt' | 'email-txt' | 'region-txt' | 'csv' | 'json'

export function serializeKiroApiKeys(keys: CreateKiroApiKeyItem[], format: KiroKeyExportFormat = 'txt'): string {
  if (format === 'json') return JSON.stringify(keys.map(({ persisted: _persisted, warning: _warning, ...key }) => key), null, 2)
  if (format === 'csv') {
    const timestamp = (value: number) => Number.isFinite(value) && Math.abs(value) <= 8640000000000000 ? new Date(value).toISOString() : ''
    const cell = (value: string | number) => `"${String(value).replace(/"/g, '""')}"`
    return '\uFEFF' + [
      ['账号 ID', '邮箱', 'Key', '密钥名称', '服务区域', '创建时间'],
      ...keys.map((key) => [key.credentialId, key.email || '', key.apiKey, key.label, key.region, timestamp(key.createdAt)]),
    ].map((row) => row.map(cell).join(',')).join('\r\n') + '\r\n'
  }
  return keys.map((key) => format === 'email-txt' ? `${key.email || ''}\t${key.apiKey}`
    : format === 'region-txt' ? `${key.apiKey}----${key.region}` : key.apiKey).join('\n') + (keys.length ? '\n' : '')
}

/** 每次只提交一个账号，让前面已经返回的完整 Key 可立即展示和导出。 */
export async function runKiroApiKeyBatch(
  credentialIds: number[],
  labelPrefix: string,
  create: (credentialId: number, labelPrefix: string) => Promise<BatchCreateKiroApiKeysResponse>,
  onItem: (result: BatchCreateKiroApiKeysResponse) => void,
  shouldStop: () => boolean,
): Promise<void> {
  for (const id of new Set(credentialIds)) {
    if (shouldStop()) break
    let result: BatchCreateKiroApiKeysResponse
    try {
      result = await create(id, labelPrefix)
    } catch (error) {
      result = {
        total: 1, success: 0, skipped: 0, failed: 1, keys: [],
        items: [{ credentialId: id, ok: false, skipped: false, error: error instanceof Error ? error.message : '创建请求失败' }],
      }
    }
    onItem(result)
  }
}
