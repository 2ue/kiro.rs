/**
 * 运行配置字段注册表：每个可编辑字段只在这里定义一次，
 * 配置页、外部池策略页、命令面板搜索都从这里读取。
 */

export type Getter = (path: string) => unknown

export interface FieldBase {
  path: string
  label: string
  desc?: string
  /** 默认折叠到"高级"里 */
  advanced?: boolean
  disabledWhen?: (get: Getter) => boolean
}

export type FieldDef =
  | (FieldBase & { kind: 'number'; min?: number; max?: number; step?: number; suffix?: string })
  | (FieldBase & { kind: 'bool' })
  | (FieldBase & { kind: 'select'; options: Array<{ value: string; label: string }> })
  | (FieldBase & { kind: 'list'; placeholder?: string })
  | (FieldBase & { kind: 'codes' })
  | (FieldBase & { kind: 'text'; multiline?: boolean })

export interface FieldGroup {
  id: string
  title: string
  description?: string
  fields: FieldDef[]
}

export interface SettingsSection {
  id: string
  title: string
  description: string
  groups: FieldGroup[]
}
