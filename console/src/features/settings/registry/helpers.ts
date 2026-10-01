import type { FieldBase, FieldDef, Getter } from './types'

export const num = (path: string, label: string, desc: string, opts: Partial<Extract<FieldDef, { kind: 'number' }>> = {}): FieldDef => ({
  kind: 'number',
  path,
  label,
  desc,
  ...opts,
})
export const bool = (path: string, label: string, desc = '', opts: Partial<FieldBase> = {}): FieldDef => ({
  kind: 'bool',
  path,
  label,
  desc,
  ...opts,
})
export const sel = (
  path: string,
  label: string,
  desc: string,
  options: Array<[string, string]>,
  opts: Partial<FieldBase> = {},
): FieldDef => ({
  kind: 'select',
  path,
  label,
  desc,
  options: options.map(([value, l]) => ({ value, label: l })),
  ...opts,
})
export const off = (path: string) => (get: Getter) => !get(path)

export const EP = 'externalPools.'
export const ROUTE_MODES: Array<[string, string]> = [
  ['allow_all', '全部入口'],
  ['allow_list', '只允许下列入口'],
  ['deny_list', '禁止下列入口'],
]
