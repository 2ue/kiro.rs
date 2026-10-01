/**
 * URL search 参数校验：每个字段可选，非法值回落为 undefined，坏链接不会让页面报错。
 * 只覆盖路由需要的几种类型，避免为此引入完整的 schema 库进入首屏包。
 */

type Field<T> = (raw: unknown) => T | undefined

export const opt = {
  str: (): Field<string> => (v) => (typeof v === 'string' ? v : typeof v === 'number' ? String(v) : undefined),
  int:
    (min?: number): Field<number> =>
    (v) => {
      const n = typeof v === 'string' && v.trim() !== '' ? Number(v) : v
      return typeof n === 'number' && Number.isInteger(n) && (min === undefined || n >= min) ? n : undefined
    },
  num: (): Field<number> => (v) => {
    const n = typeof v === 'string' && v.trim() !== '' ? Number(v) : v
    return typeof n === 'number' && Number.isFinite(n) ? n : undefined
  },
  bool: (): Field<boolean> => (v) => (v === true || v === 'true' ? true : v === false || v === 'false' ? false : undefined),
  enum:
    <const T extends readonly string[]>(values: T): Field<T[number]> =>
    (v) =>
      typeof v === 'string' && (values as readonly string[]).includes(v) ? (v as T[number]) : undefined,
}

export type SearchOf<S extends Record<string, Field<unknown>>> = { [K in keyof S]?: S[K] extends Field<infer T> ? T : never }

/** 组装为 TanStack Router 的 validateSearch 函数 */
export function searchSchema<S extends Record<string, Field<unknown>>>(shape: S) {
  return (input: Record<string, unknown>): SearchOf<S> => {
    const out: Record<string, unknown> = {}
    for (const key of Object.keys(shape)) {
      const value = shape[key]!(input[key])
      if (value !== undefined) out[key] = value
    }
    return out as SearchOf<S>
  }
}
