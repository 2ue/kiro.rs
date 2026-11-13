// 导入数据的宽松取值：字段名不区分驼峰 / 下划线 / 大小写，数字和布尔可以是字符串。

export type JsonObject = Record<string, unknown>

export function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** `refresh_token`、`refreshToken`、`RefreshToken` 都归一成 `refreshtoken`。 */
function compactKey(key: string): string {
  return key.toLowerCase().replace(/[^a-z0-9]/g, '')
}

/** 按任一别名取字段，别名之间按先后顺序优先。 */
export function pick(obj: JsonObject | undefined, ...names: string[]): unknown {
  if (!obj) return undefined
  const wanted = names.map(compactKey)
  const entries = Object.entries(obj)
  for (const name of wanted) {
    for (const [key, value] of entries) {
      if (compactKey(key) === name && value !== undefined && value !== null && value !== '') return value
    }
  }
  return undefined
}

export function pickObject(obj: JsonObject | undefined, ...names: string[]): JsonObject | undefined {
  const value = pick(obj, ...names)
  return isObject(value) ? value : undefined
}

export function asString(value: unknown): string | undefined {
  if (typeof value === 'string') return value.trim() || undefined
  if (typeof value === 'number' && Number.isFinite(value)) return String(value)
  return undefined
}

/** 非负整数；`"3"`、`3.0`、`"3.0"` 都接受。 */
export function asCount(value: unknown): number | undefined {
  const parsed = typeof value === 'number' ? value : typeof value === 'string' && value.trim() ? Number(value.trim()) : NaN
  return Number.isFinite(parsed) && parsed >= 0 ? Math.trunc(parsed) : undefined
}

export function asBoolean(value: unknown): boolean | undefined {
  if (typeof value === 'boolean') return value
  if (typeof value === 'number') return value !== 0
  if (typeof value === 'string') {
    const compact = value.trim().toLowerCase()
    if (['true', '1', 'yes', 'on', 'y'].includes(compact)) return true
    if (['false', '0', 'no', 'off', 'n'].includes(compact)) return false
  }
  return undefined
}

/** 过期时间：ISO 字符串原样保留；秒或毫秒时间戳（数字或数字字符串）转成 ISO。 */
export function asTimestamp(value: unknown): string | undefined {
  const numeric = typeof value === 'number' ? value : typeof value === 'string' && /^\d+(\.\d+)?$/.test(value.trim()) ? Number(value.trim()) : undefined
  if (numeric !== undefined) {
    if (!Number.isFinite(numeric) || numeric <= 0) return undefined
    const date = new Date(numeric > 10_000_000_000 ? numeric : numeric * 1000)
    return Number.isFinite(date.getTime()) ? date.toISOString() : undefined
  }
  return asString(value)
}

export function asStringList(value: unknown): string[] {
  const items = Array.isArray(value) ? value : typeof value === 'string' ? value.split(/[,\n]/) : []
  return items
    .map((item) => asString(item))
    .filter((item): item is string => Boolean(item))
}
