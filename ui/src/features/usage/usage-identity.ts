import type { UsageRecord } from '@/types/api'

export function accountIdentityLabel(id: number, name?: string): string {
  const text = name?.trim()
  if (!text || text === `#${id}`) return `#${id}`
  return text.startsWith(`#${id} `) ? text : `#${id} ${text}`
}

export function usageRecordAccountId(record: Pick<UsageRecord, 'accountId' | 'externalAttempts'>): number | undefined {
  const attempts = record.externalAttempts || []
  return record.accountId ?? attempts[attempts.length - 1]?.poolId
}

export function usageRecordAccountIdentity(
  record: Pick<UsageRecord, 'accountId' | 'accountName' | 'externalAttempts'>,
): string {
  const id = usageRecordAccountId(record)
  if (id == null) return '-'
  if (record.accountName?.trim()) return accountIdentityLabel(id, record.accountName)
  const attempts = record.externalAttempts || []
  for (let index = attempts.length - 1; index >= 0; index -= 1) {
    const attempt = attempts[index]
    if (attempt.poolId === id && attempt.poolName?.trim()) return accountIdentityLabel(id, attempt.poolName)
  }
  return accountIdentityLabel(id)
}
