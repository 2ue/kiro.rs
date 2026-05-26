import camelCase from 'lodash-es/camelCase'

type JsonRecord = Record<string, unknown>

function isPlainRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function camelizeKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(camelizeKeys)
  if (!isPlainRecord(value)) return value

  return Object.fromEntries(
    Object.entries(value).map(([key, item]) => [camelCase(key), camelizeKeys(item)])
  )
}
