import type { ExternalPool, ExternalPoolModelMappingRule, ExternalPoolsConfig, CreateExternalPoolRequest } from '@/types/api'

export const splitRules = (value: string) => value.split('\n').map((item) => item.trim()).filter(Boolean)
export const joinRules = (value: string[] = []) => value.join('\n')
export const whole = (value: number, min = 0) => Math.max(min, Math.floor(Number.isFinite(value) ? value : min))

export const DEFAULT_POOL_MODEL_MAPPING_MODE: NonNullable<CreateExternalPoolRequest['modelMappingMode']> = 'processed_mapping'

export const parseModelMappingRules = (value: string): ExternalPoolModelMappingRule[] => value
  .split('\n')
  .map((line) => line.trim())
  .filter((line) => line && !line.startsWith('#') && !line.startsWith('//'))
  .map((line) => line.split(/\s*(?:->|=>|→|=)\s*/, 2))
  .map(([source, target]) => ({
    enabled: true,
    source: source?.trim() || '',
    target: target?.trim() || '',
    kind: 'alias' as const,
  }))
  .filter((rule) => rule.source && rule.target)

export const joinModelMappingRules = (rules: ExternalPoolModelMappingRule[] = []) => rules
  .filter((rule) => rule.source?.trim() && rule.target?.trim())
  .map((rule) => `${rule.source.trim()} -> ${rule.target.trim()}`)
  .join('\n')

export type ExternalPoolModelMappingPreset = {
  label: string
  source: string
  target: string
  tone: 'blue' | 'cyan' | 'emerald' | 'purple' | 'amber' | 'rose'
}

export const DIRECT_MODEL_MAPPING_PRESETS: ExternalPoolModelMappingPreset[] = [
  { label: 'Sonnet 4 完整ID→4', source: 'claude-sonnet-4-20250514', target: 'claude-sonnet-4', tone: 'blue' },
  { label: 'Sonnet 4 原样', source: 'claude-sonnet-4', target: 'claude-sonnet-4', tone: 'blue' },
  { label: 'Sonnet 4.5 完整ID→4.5', source: 'claude-sonnet-4-5-20250929', target: 'claude-sonnet-4.5', tone: 'blue' },
  { label: 'Sonnet 4.5→4.5', source: 'claude-sonnet-4-5', target: 'claude-sonnet-4.5', tone: 'blue' },
  { label: 'Sonnet 4.6→4.6', source: 'claude-sonnet-4-6', target: 'claude-sonnet-4.6', tone: 'cyan' },
  { label: 'Sonnet 4.6 点号', source: 'claude-sonnet-4.6', target: 'claude-sonnet-4.6', tone: 'cyan' },
  { label: 'Sonnet 4.7→4.7', source: 'claude-sonnet-4-7', target: 'claude-sonnet-4.7', tone: 'cyan' },
  { label: 'Sonnet 4.8→4.8', source: 'claude-sonnet-4-8', target: 'claude-sonnet-4.8', tone: 'cyan' },
  { label: 'Opus 4.5 完整ID→4.5', source: 'claude-opus-4-5-20251101', target: 'claude-opus-4.5', tone: 'purple' },
  { label: 'Opus 4.5→4.5', source: 'claude-opus-4-5', target: 'claude-opus-4.5', tone: 'purple' },
  { label: 'Opus 4.6→4.6', source: 'claude-opus-4-6', target: 'claude-opus-4.6', tone: 'purple' },
  { label: 'Opus 4.7→4.7', source: 'claude-opus-4-7', target: 'claude-opus-4.7', tone: 'purple' },
  { label: 'Opus 4.8→4.8', source: 'claude-opus-4-8', target: 'claude-opus-4.8', tone: 'purple' },
  { label: 'Haiku 4.5 完整ID→4.5', source: 'claude-haiku-4-5-20251001', target: 'claude-haiku-4.5', tone: 'emerald' },
  { label: 'Haiku 4.5→4.5', source: 'claude-haiku-4-5', target: 'claude-haiku-4.5', tone: 'emerald' },
  { label: '3.5 Sonnet 完整ID', source: 'claude-3-5-sonnet-20241022', target: 'claude-3.5-sonnet', tone: 'amber' },
  { label: '3.5 Haiku 完整ID', source: 'claude-3-5-haiku-20241022', target: 'claude-3.5-haiku', tone: 'emerald' },
]

export const PROCESSED_MODEL_MAPPING_PRESETS: ExternalPoolModelMappingPreset[] = [
  { label: 'Sonnet 4 原样', source: 'claude-sonnet-4', target: 'claude-sonnet-4', tone: 'blue' },
  { label: 'Sonnet 4.5→4-5', source: 'claude-sonnet-4.5', target: 'claude-sonnet-4-5', tone: 'cyan' },
  { label: 'Sonnet 4-5 原样', source: 'claude-sonnet-4-5', target: 'claude-sonnet-4-5', tone: 'cyan' },
  { label: 'Sonnet 4.6→4-6', source: 'claude-sonnet-4.6', target: 'claude-sonnet-4-6', tone: 'cyan' },
  { label: 'Sonnet 4-6 原样', source: 'claude-sonnet-4-6', target: 'claude-sonnet-4-6', tone: 'cyan' },
  { label: 'Sonnet 4.7→4-7', source: 'claude-sonnet-4.7', target: 'claude-sonnet-4-7', tone: 'cyan' },
  { label: 'Sonnet 4.8→4-8', source: 'claude-sonnet-4.8', target: 'claude-sonnet-4-8', tone: 'cyan' },
  { label: 'Opus 4.5→4-5', source: 'claude-opus-4.5', target: 'claude-opus-4-5', tone: 'purple' },
  { label: 'Opus 4-5 原样', source: 'claude-opus-4-5', target: 'claude-opus-4-5', tone: 'purple' },
  { label: 'Opus 4.6→4-6', source: 'claude-opus-4.6', target: 'claude-opus-4-6', tone: 'purple' },
  { label: 'Opus 4.7→4-7', source: 'claude-opus-4.7', target: 'claude-opus-4-7', tone: 'purple' },
  { label: 'Opus 4.8→4-8', source: 'claude-opus-4.8', target: 'claude-opus-4-8', tone: 'purple' },
  { label: 'Haiku 4.5→4-5', source: 'claude-haiku-4.5', target: 'claude-haiku-4-5', tone: 'emerald' },
  { label: 'Haiku 4-5 原样', source: 'claude-haiku-4-5', target: 'claude-haiku-4-5', tone: 'emerald' },
  { label: '3.5 Sonnet→3-5', source: 'claude-3.5-sonnet', target: 'claude-3-5-sonnet', tone: 'amber' },
  { label: '3.5 Haiku→3-5', source: 'claude-3.5-haiku', target: 'claude-3-5-haiku', tone: 'emerald' },
]

export type ExternalPoolFormDraft = {
  name: string
  baseUrl: string
  apiKey: string
  authType: NonNullable<CreateExternalPoolRequest['authType']>
  enabled: boolean
  priority: number
  maxConcurrentRequests: number
  usageProjectionMode: NonNullable<CreateExternalPoolRequest['usageProjectionMode']>
  autoDisablePolicy: NonNullable<CreateExternalPoolRequest['autoDisablePolicy']>
  preservePath: boolean
  normalizeModelVersionDots: boolean
  modelMappingMode: NonNullable<CreateExternalPoolRequest['modelMappingMode']>
  modelMappingRequireMatch: boolean
  modelMappingRulesText: string
  notes: string
}

export const defaultPoolForm = (): ExternalPoolFormDraft => ({
  name: '',
  baseUrl: '',
  apiKey: '',
  authType: 'bearer',
  enabled: false,
  priority: 100,
  maxConcurrentRequests: 10,
  usageProjectionMode: 'pass_through',
  autoDisablePolicy: 'inherit',
  preservePath: true,
  normalizeModelVersionDots: false,
  modelMappingMode: DEFAULT_POOL_MODEL_MAPPING_MODE,
  modelMappingRequireMatch: false,
  modelMappingRulesText: '',
  notes: '',
})

export const poolFormFromPool = (pool: ExternalPool): ExternalPoolFormDraft => ({
  name: pool.name,
  baseUrl: pool.baseUrl,
  apiKey: '',
  authType: pool.authType,
  enabled: pool.enabled,
  priority: pool.priority,
  maxConcurrentRequests: pool.maxConcurrentRequests,
  usageProjectionMode: pool.usageProjectionMode,
  autoDisablePolicy: pool.autoDisablePolicy,
  preservePath: pool.preservePath !== false,
  normalizeModelVersionDots: Boolean(pool.normalizeModelVersionDots),
  modelMappingMode: pool.modelMappingMode || DEFAULT_POOL_MODEL_MAPPING_MODE,
  modelMappingRequireMatch: Boolean(pool.modelMappingRequireMatch),
  modelMappingRulesText: joinModelMappingRules(pool.modelMappingRules || []),
  notes: pool.notes || '',
})

export function modelMappingPresetsForMode(mode: ExternalPoolFormDraft['modelMappingMode']): ExternalPoolModelMappingPreset[] {
  if (mode === 'passthrough_mapping') return DIRECT_MODEL_MAPPING_PRESETS
  if (mode === 'direct_mapping') return DIRECT_MODEL_MAPPING_PRESETS
  if (mode === 'processed_mapping') return PROCESSED_MODEL_MAPPING_PRESETS
  return []
}

export function appendModelMappingRules(currentText: string, incomingRules: ExternalPoolModelMappingRule[]): { text: string; added: number } {
  const rules = parseModelMappingRules(currentText)
  const seen = new Set(rules.map((rule) => rule.source.trim().toLowerCase()))
  let added = 0
  incomingRules.forEach((rule) => {
    const source = rule.source?.trim() || ''
    const target = rule.target?.trim() || ''
    const key = source.toLowerCase()
    if (!source || !target || seen.has(key)) return
    seen.add(key)
    rules.push({ enabled: true, source, target, kind: 'alias' })
    added += 1
  })
  return { text: joinModelMappingRules(rules), added }
}

export function appendModelMappingPreset(currentText: string, preset: ExternalPoolModelMappingPreset): { text: string; added: boolean } {
  const result = appendModelMappingRules(currentText, [{ enabled: true, source: preset.source, target: preset.target, kind: 'alias' }])
  return { text: result.text, added: result.added > 0 }
}

export function appendModelMappingPresets(currentText: string, presets: ExternalPoolModelMappingPreset[]): { text: string; added: number } {
  return appendModelMappingRules(currentText, presets.map((p) => ({ enabled: true, source: p.source, target: p.target, kind: 'alias' })))
}

export function modelMappingDescription(mode: ExternalPool['modelMappingMode'] | undefined, normalizeFallback: boolean): string {
  const processedFallback = normalizeFallback ? '未命中后使用内部处理模型，并把数字点号转横杠。' : '未命中后使用内部处理模型。'
  if (mode === 'passthrough') return '直接使用客户端请求里的模型，不应用映射规则和兜底转换。'
  if (mode === 'passthrough_mapping') return '先用客户端请求模型匹配规则；未命中时仍使用原请求模型。'
  if (mode === 'direct_mapping') return `用客户端请求模型匹配规则；${processedFallback}`
  return `先使用本系统解析后的模型匹配规则；${processedFallback}`
}

export function poolModelMappingSummary(pool: ExternalPool): string {
  if (pool.modelMappingMode === 'passthrough') return '原样'
  const count = pool.modelMappingRules?.length || 0
  const mode = pool.modelMappingMode === 'passthrough_mapping'
    ? '原样+映射'
    : pool.modelMappingMode === 'direct_mapping'
      ? '映射+内部'
      : '内部+映射'
  const fallback = pool.modelMappingRequireMatch ? '必须命中' : pool.normalizeModelVersionDots ? '未命中4.8->4-8' : '允许未命中'
  return `${mode}${count ? ` ${count}条` : ''} · ${fallback}`
}

export function usageProjectionDescription(mode: ExternalPool['usageProjectionMode'] | undefined): string {
  if (mode === 'current_path_policy') {
    return '按当前入口规则整理用量，并应用全局用量补偿。适合希望外部账号展示方式和本地入口一致的场景。'
  }
  return '保持外部账号返回的用量，不应用缓存补偿和输出补偿。适合只做外部连接的场景。'
}

export function poolUsageSummary(pool: ExternalPool, config: ExternalPoolsConfig): string {
  if (pool.usageProjectionMode !== 'current_path_policy') {
    return '用量：保持原样'
  }
  const parts = ['用量：按入口规则']
  if (config.externalPoolUsageProjectionUpliftPercent > 0) {
    parts.push(`缓存 +${config.externalPoolUsageProjectionUpliftPercent}%`)
  }
  if (config.externalPoolUsageProjectionOutputUpliftMinTokens > 0 && config.externalPoolUsageProjectionOutputUpliftPercent > 0) {
    parts.push(`输出 >= ${config.externalPoolUsageProjectionOutputUpliftMinTokens} 后 +${config.externalPoolUsageProjectionOutputUpliftPercent}%`)
  }
  return parts.join(' · ')
}

export function authLabel(authType: ExternalPool['authType']): string {
  return authType === 'x_api_key' ? 'x-api-key' : 'Bearer'
}

export function modelMappingPresetClass(tone: ExternalPoolModelMappingPreset['tone']): string {
  switch (tone) {
    case 'cyan': return 'bg-cyan-100 text-cyan-700 hover:bg-cyan-200 dark:bg-cyan-900/30 dark:text-cyan-300 dark:hover:bg-cyan-900/50'
    case 'emerald': return 'bg-emerald-100 text-emerald-700 hover:bg-emerald-200 dark:bg-emerald-900/30 dark:text-emerald-300 dark:hover:bg-emerald-900/50'
    case 'purple': return 'bg-purple-100 text-purple-700 hover:bg-purple-200 dark:bg-purple-900/30 dark:text-purple-300 dark:hover:bg-purple-900/50'
    case 'amber': return 'bg-amber-100 text-amber-700 hover:bg-amber-200 dark:bg-amber-900/30 dark:text-amber-300 dark:hover:bg-amber-900/50'
    case 'rose': return 'bg-rose-100 text-rose-700 hover:bg-rose-200 dark:bg-rose-900/30 dark:text-rose-300 dark:hover:bg-rose-900/50'
    case 'blue':
    default: return 'bg-blue-100 text-blue-700 hover:bg-blue-200 dark:bg-blue-900/30 dark:text-blue-300 dark:hover:bg-blue-900/50'
  }
}
