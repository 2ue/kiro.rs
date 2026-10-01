export type * from './types'
import { CORE_SECTIONS } from './core'
import { RETRY_SECTIONS } from './retry'
import { PAYLOAD_SECTIONS } from './payload'
import { STEERING_SECTIONS } from './steering'
import { COMPAT_SECTIONS } from './compat'
import { POOL_POLICY_GROUPS } from './pools'
import type { FieldDef, SettingsSection } from './types'

export { POOL_POLICY_GROUPS }

export const SETTINGS_SECTIONS: SettingsSection[] = [
  ...CORE_SECTIONS,
  ...RETRY_SECTIONS,
  ...PAYLOAD_SECTIONS,
  ...STEERING_SECTIONS,
  ...COMPAT_SECTIONS,
]

export const ALL_FIELDS: Array<FieldDef & { section: string; sectionTitle: string; group: string }> = [
  ...SETTINGS_SECTIONS.flatMap((s) =>
    s.groups.flatMap((g) => g.fields.map((f) => ({ ...f, section: s.id, sectionTitle: s.title, group: g.title }))),
  ),
  ...POOL_POLICY_GROUPS.flatMap((g) => g.fields.map((f) => ({ ...f, section: 'pools', sectionTitle: '外部池策略', group: g.title }))),
]

export const FIELD_BY_PATH = new Map(ALL_FIELDS.map((f) => [f.path, f]))
