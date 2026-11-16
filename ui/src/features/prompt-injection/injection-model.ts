import type { InjectionCatalogEntry } from '@/types/api'
export const CATEGORY_LABEL: Record<InjectionCatalogEntry['category'], string> = { operator:'运营引导',protocol:'协议兼容',feature:'功能附带',recovery:'故障恢复' }
export const BLOCK_REASON_LABEL: Record<string,string> = { off:'开关关闭',strict:'严格模式' }
export const THINKING_TRIGGER_LABEL = { off:'关闭',real_request:'按请求触发',always:'总是开启' }

export function injectionSetting(defaults: Partial<Record<string, { enabled?: boolean }>>, route: Partial<Record<string, { enabled?: boolean }>>, id: string): { enabled: boolean; source: 'route' | 'global_template' | 'builtin_default' } {
  if (route[id]?.enabled !== undefined) return { enabled: route[id]!.enabled!, source: 'route' }
  if (defaults[id]?.enabled !== undefined) return { enabled: defaults[id]!.enabled!, source: 'global_template' }
  return { enabled: false, source: 'builtin_default' }
}
