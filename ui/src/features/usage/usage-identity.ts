import type { UsageRecord } from '@/types/api'

export function credentialIdentityLabel(id: number, label?: string): string {
  const trimmed = label?.trim()
  if (!trimmed) return `#${id}`
  const name = trimmed.replace(new RegExp(`^#${id}\\s*`), '')
  return name ? `#${id} ${name}` : `#${id}`
}

export function usageRecordCredentialIdentity(
  record: Pick<UsageRecord, 'credentialId' | 'credentialLabel' | 'credentialAttempts'>,
): string {
  const attempts = record.credentialAttempts || []
  const id = record.credentialId ?? attempts[attempts.length - 1]?.credentialId
  if (id == null) return '-'

  let attemptLabel: string | undefined
  for (let index = attempts.length - 1; index >= 0; index -= 1) {
    const attempt = attempts[index]
    if (attempt.credentialId === id && attempt.credentialLabel?.trim()) {
      attemptLabel = attempt.credentialLabel
      break
    }
  }

  return credentialIdentityLabel(id, record.credentialLabel?.trim() || attemptLabel)
}
