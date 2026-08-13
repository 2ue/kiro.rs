const REQUEST_API_KEY_ID_PATTERN = /^[a-f0-9]{64}$/i

export function normalizeRequestApiKeyId(value?: string | null): string | undefined {
  const trimmed = value?.trim()
  if (!trimmed || !REQUEST_API_KEY_ID_PATTERN.test(trimmed)) return undefined
  return trimmed.toLowerCase()
}

export function formatRequestApiKeyId(value?: string | null): string {
  const normalized = normalizeRequestApiKeyId(value)
  if (!normalized) return '-'
  return `${normalized.slice(0, 8)}...${normalized.slice(-8)}`
}
