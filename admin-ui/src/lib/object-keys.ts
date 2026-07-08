type JsonRecord = Record<string, unknown>

function isPlainRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function camelCaseKey(key: string): string {
  return key.replace(/[-_]+([a-zA-Z0-9])/g, (_, char: string) => char.toUpperCase())
}

export function camelizeKeys(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(camelizeKeys)
  }
  if (!isPlainRecord(value)) {
    return value
  }

  return Object.fromEntries(
    Object.entries(value).map(([key, item]) => [camelCaseKey(key), camelizeKeys(item)])
  )
}
