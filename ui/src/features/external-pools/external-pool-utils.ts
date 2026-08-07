import type { ExternalPool, ExternalPoolModelMappingRule, ExternalPoolsConfig, CreateExternalPoolRequest, ExternalPoolStreamResponseMode, ExternalPoolStreamRetryMode } from '@/types/api'

export const splitRules = (value: string) => value.split('\n').map((item) => item.trim()).filter(Boolean)
export const joinRules = (value: string[] = []) => value.join('\n')
export const whole = (value: number, min = 0) => Math.max(min, Math.floor(Number.isFinite(value) ? value : min))

export const parseStatusCodeList = (value: string): number[] => {
  const seen = new Set<number>()
  const codes: number[] = []
  for (const raw of value.split(/[\s,，;；]+/)) {
    const text = raw.trim()
    if (!text) continue
    const code = Number(text)
    if (!Number.isInteger(code) || code < 100 || code > 599 || seen.has(code)) continue
    seen.add(code)
    codes.push(code)
  }
  return codes
}

export const joinStatusCodeList = (value: number[] = []) => value
  .filter((code) => Number.isInteger(code) && code >= 100 && code <= 599)
  .join(', ')

export const parseSupportedModelsText = (value: string): string[] => {
  const seen = new Set<string>()
  const models: string[] = []
  for (const item of value.split(/[\s,，;；]+/)) {
    const model = item.trim()
    if (!model) continue
    const key = model.toLowerCase()
    if (seen.has(key)) continue
    seen.add(key)
    models.push(model)
  }
  return models
}

export const DEFAULT_POOL_MODEL_MAPPING_MODE: NonNullable<CreateExternalPoolRequest['modelMappingMode']> = 'processed_mapping'
export type ExternalPoolStreamResponseDraft = ExternalPoolStreamResponseMode | 'inherit'
export type ExternalPoolStreamRetryDraft = ExternalPoolStreamRetryMode

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
  requestBodyMode: NonNullable<CreateExternalPoolRequest['requestBodyMode']>
  rawModelMode: NonNullable<CreateExternalPoolRequest['rawModelMode']>
  usageProjectionMode: NonNullable<CreateExternalPoolRequest['usageProjectionMode']>
  streamResponseMode: ExternalPoolStreamResponseDraft
  preOutputStreamRetryMode: ExternalPoolStreamRetryDraft
  autoDisablePolicy: NonNullable<CreateExternalPoolRequest['autoDisablePolicy']>
  preservePath: boolean
  normalizeModelVersionDots: boolean
  supportedModelsText: string
  routeMode: NonNullable<CreateExternalPoolRequest['routeMode']>
  routeRulesText: string
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
  requestBodyMode: 'normalized',
  rawModelMode: 'none',
  usageProjectionMode: 'pass_through',
  streamResponseMode: 'inherit',
  preOutputStreamRetryMode: 'inherit',
  autoDisablePolicy: 'inherit',
  preservePath: true,
  normalizeModelVersionDots: false,
  supportedModelsText: '',
  routeMode: 'allow_all',
  routeRulesText: '',
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
  requestBodyMode: pool.requestBodyMode || 'normalized',
  rawModelMode: pool.rawModelMode || 'none',
  usageProjectionMode: pool.usageProjectionMode,
  streamResponseMode: pool.streamResponseMode || 'inherit',
  preOutputStreamRetryMode: pool.preOutputStreamRetryMode || 'inherit',
  autoDisablePolicy: pool.autoDisablePolicy,
  preservePath: pool.preservePath !== false,
  normalizeModelVersionDots: Boolean(pool.normalizeModelVersionDots),
  supportedModelsText: joinRules(pool.supportedModels || []),
  routeMode: pool.routeMode || 'allow_all',
  routeRulesText: joinRules(pool.routeRules || []),
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

export function requestBodyModeDescription(mode: ExternalPool['requestBodyMode'] | undefined): string {
  if (mode === 'raw_passthrough') {
    return '请求体不进入本系统的消息解析、图片处理、schema 修正和 payload guard。下游 usage 是透传上游还是按入口路径整理，由当前外部账号的下游 usage 口径决定；是否改写顶层 model 由下方模型处理配置单独控制。'
  }
  return '按当前系统的标准 Anthropic 请求处理链路转发，会应用图片预处理、payload guard、thinking/model 兼容逻辑和 usage 整形上下文。'
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

export function poolBodyModeSummary(pool: ExternalPool): string {
  if (pool.requestBodyMode === 'raw_passthrough') {
    return pool.rawModelMode === 'rewrite_top_level'
      ? 'Body：raw透传+模型处理'
      : 'Body：raw透传'
  }
  return 'Body：标准处理'
}

export function poolSupportedModelsSummary(pool: ExternalPool): string {
  const models = pool.supportedModels || []
  if (models.length === 0) return '支持：不限制'
  if (models.length <= 2) return `支持：${models.join(', ')}`
  return `支持：${models[0]}, ${models[1]} 等 ${models.length} 个`
}

export function poolRouteSummary(pool: ExternalPool): string {
  const rules = pool.routeRules || []
  if (!pool.routeMode || pool.routeMode === 'allow_all') return '入口：不限制'
  if (pool.routeMode === 'allow_list') {
    if (rules.length === 0) return '入口：不允许'
    return rules.length <= 2
      ? `入口：仅 ${rules.join(', ')}`
      : `入口：仅 ${rules[0]}, ${rules[1]} 等 ${rules.length} 条`
  }
  if (rules.length === 0) return '入口：不限制'
  return rules.length <= 2
    ? `入口：排除 ${rules.join(', ')}`
    : `入口：排除 ${rules[0]}, ${rules[1]} 等 ${rules.length} 条`
}

export function usageProjectionDescription(mode: ExternalPool['usageProjectionMode'] | undefined): string {
  if (mode === 'current_path_policy') {
    return '返回给下游的 usage 按当前入口路径的缓存策略整理；如果该路径是 no-cache 或非流式 Usage 透传，则保持上游原始 usage。'
  }
  return '返回给下游的 usage 保持外部账号上游原始值；不按当前入口路径整理。'
}

export function streamResponseDescription(mode: ExternalPoolStreamResponseDraft): string {
  if (mode === 'event_passthrough') {
    return '仅控制 stream=true 的 SSE 事件转发方式。文本、thinking、tool 等普通事件按上游事件级转发；usage 是否透传上游或按路径整理，由上面的下游 usage 口径决定。'
  }
  return '当前外部账号不单独指定流式 SSE 转发方式，使用外部池全局默认值；usage 口径仍由上面的下游 usage 口径决定。'
}

export function streamRetryDescription(mode: ExternalPoolStreamRetryDraft): string {
  if (mode === 'enabled') {
    return '当前外部账号强制启用：stream 在 message_start、ping 等协议前缀后遇到 error、断流、EOF 或空闲超时时，可在未提交内容前换其他外部账号。'
  }
  if (mode === 'disabled') {
    return '当前外部账号强制关闭：stream body 内错误保持现有流错误行为，不把本次请求重放到其他外部账号。'
  }
  return '当前外部账号继承全局“流式首输出前错误换池”开关。'
}

export function streamRetrySummary(pool: ExternalPool, config: ExternalPoolsConfig): string {
  const mode = pool.preOutputStreamRetryMode || 'inherit'
  if (mode === 'enabled') return '首输出恢复：启用'
  if (mode === 'disabled') return '首输出恢复：禁用'
  return config.externalPoolStreamPreOutputRetryEnabled
    ? '首输出恢复：继承启用'
    : '首输出恢复：继承禁用'
}

export function poolUsageSummary(pool: ExternalPool, config: ExternalPoolsConfig): string {
  const parts = pool.usageProjectionMode === 'current_path_policy'
    ? ['Usage：按路径整理']
    : ['Usage：透传上游']
  const streamMode = pool.streamResponseMode || config.externalPoolStreamResponseMode
  parts.push(streamMode === 'event_passthrough' ? '流式：事件透传' : '流式：继承默认')
  if (pool.streamResponseMode) {
    parts.push('单池覆盖')
  }
  if (pool.usageProjectionMode !== 'current_path_policy') {
    return parts.join(' · ')
  }
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
