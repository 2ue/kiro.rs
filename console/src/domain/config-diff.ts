/** 运行配置草稿的路径式读写、字段级 diff 与三方合并 */

export type Path = string

export function getAt(obj: unknown, path: Path): unknown {
  let cur: unknown = obj
  for (const key of path.split('.')) {
    if (cur === null || typeof cur !== 'object') return undefined
    cur = (cur as Record<string, unknown>)[key]
  }
  return cur
}

export function setAt<T>(obj: T, path: Path, value: unknown): T {
  const keys = path.split('.')
  const clone = (v: unknown) => (Array.isArray(v) ? [...v] : v && typeof v === 'object' ? { ...(v as object) } : {})
  const root = clone(obj) as Record<string, unknown>
  let cur = root
  keys.forEach((key, i) => {
    if (i === keys.length - 1) cur[key] = value
    else {
      cur[key] = clone(cur[key])
      cur = cur[key] as Record<string, unknown>
    }
  })
  return root as T
}

export function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true
  if (typeof a !== typeof b || a === null || b === null || typeof a !== 'object') return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  const ka = Object.keys(a as object)
  const kb = Object.keys(b as object)
  if (ka.length !== kb.length) return false
  return ka.every((k) => deepEqual((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]))
}

export interface FieldChange {
  path: Path
  before: unknown
  after: unknown
}

/** 列出叶子字段的差异；数组按整体比较 */
export function diff(base: unknown, next: unknown, prefix = ''): FieldChange[] {
  if (deepEqual(base, next)) return []
  const isObj = (v: unknown) => v !== null && typeof v === 'object' && !Array.isArray(v)
  if (!isObj(base) || !isObj(next)) return [{ path: prefix, before: base, after: next }]
  const keys = new Set([...Object.keys(base as object), ...Object.keys(next as object)])
  const out: FieldChange[] = []
  for (const k of keys) {
    out.push(...diff((base as Record<string, unknown>)[k], (next as Record<string, unknown>)[k], prefix ? `${prefix}.${k}` : k))
  }
  return out
}

export interface MergeResult<T> {
  merged: T
  /** 本地和远端都修改且不一致的字段 */
  conflicts: FieldChange[]
}

/**
 * 三方合并：以最新远端为基础，叠加用户相对打开时的修改。
 * 远端同时改了同一字段且值不同则记为冲突（保留用户值，由界面提示）。
 */
export function merge3<T>(base: T, remote: T, draft: T): MergeResult<T> {
  const local = diff(base, draft)
  const remoteChanges = new Map(diff(base, remote).map((c) => [c.path, c]))
  let merged = remote
  const conflicts: FieldChange[] = []
  for (const change of local) {
    const r = remoteChanges.get(change.path)
    if (r && !deepEqual(r.after, change.after)) conflicts.push({ path: change.path, before: r.after, after: change.after })
    merged = setAt(merged, change.path, change.after)
  }
  return { merged, conflicts }
}
