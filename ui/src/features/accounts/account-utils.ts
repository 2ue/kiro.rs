import type { Account,AccountModelMappingRule,AccountsConfig,CreateAccountRequest,StreamResponseMode,StreamRetryMode } from '@/types/api'

export const splitRules = (value: string) => value.split('\n').map((item) => item.trim()).filter(Boolean)
export const joinRules = (value: string[] = []) => value.join('\n')
export const whole = (value: number, min = 0) => Math.max(min, Math.floor(Number.isFinite(value) ? value : min))

export const parseHeaderOverridesText = (value: string): Record<string, string> => {
  const headers: Record<string, string> = {}
  for (const rawLine of value.split('\n')) {
    const line = rawLine.trim()
    if (!line || line.startsWith('#') || line.startsWith('//')) continue
    const separatorIndex = line.indexOf(':') >= 0 ? line.indexOf(':') : line.indexOf('=')
    if (separatorIndex <= 0) continue
    const name = line.slice(0, separatorIndex).trim().toLowerCase()
    const headerValue = line.slice(separatorIndex + 1).trim()
    if (!name || !headerValue) continue
    headers[name] = headerValue
  }
  return headers
}

export const joinHeaderOverrides = (value: Record<string, string> = {}) => Object.entries(value)
  .filter(([name, headerValue]) => name.trim() && headerValue.trim())
  .map(([name, headerValue]) => `${name.trim().toLowerCase()}: ${headerValue.trim()}`)
  .join('\n')

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

export const DEFAULT_POOL_MODEL_MAPPING_MODE: NonNullable<CreateAccountRequest['modelMappingMode']> = 'processed_mapping'
export type AccountStreamResponseDraft = StreamResponseMode | 'inherit'
export type AccountStreamRetryDraft = StreamRetryMode

export const parseModelMappingRules = (value: string): AccountModelMappingRule[] => value
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

export const joinModelMappingRules = (rules: AccountModelMappingRule[] = []) => rules
  .filter((rule) => rule.source?.trim() && rule.target?.trim())
  .map((rule) => `${rule.source.trim()} -> ${rule.target.trim()}`)
  .join('\n')

export type AccountModelMappingPreset = {
  label: string
  source: string
  target: string
  tone: 'blue' | 'cyan' | 'emerald' | 'purple' | 'amber' | 'rose'
}

export const DIRECT_MODEL_MAPPING_PRESETS: AccountModelMappingPreset[] = [
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

export const PROCESSED_MODEL_MAPPING_PRESETS: AccountModelMappingPreset[] = [
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

export type AccountFormDraft = {
 tagsText: string
 proxyUrl: string
 costMultiplier: number
 overrides: import('@/types/api').AccountOverrides
  name: string
  baseUrl: string
  apiKey: string
  authType: NonNullable<CreateAccountRequest['authType']>
  headerProfile: NonNullable<CreateAccountRequest['headerProfile']>
  appendBetaQuery: boolean
  headerOverridesText: string
  wireProfile: NonNullable<CreateAccountRequest['wireProfile']>
  tlsProfile: NonNullable<CreateAccountRequest['tlsProfile']>
  enabled: boolean
  priority: number
  maxConcurrentRequests: number
  usageProjectionMode: NonNullable<CreateAccountRequest['usageProjectionMode']>
  streamResponseMode: AccountStreamResponseDraft
  preOutputStreamRetryMode: AccountStreamRetryDraft
  autoDisablePolicy: NonNullable<CreateAccountRequest['autoDisablePolicy']>
  normalizeModelVersionDots: boolean
  supportedModelsText: string
  routeMode: NonNullable<CreateAccountRequest['routeMode']>
  routeRules: string[]
  modelMappingMode: NonNullable<CreateAccountRequest['modelMappingMode']>
  modelMappingRequireMatch: boolean
  modelMappingRulesText: string
  notes: string
}

export const defaultAccountForm = (): AccountFormDraft => ({
 tagsText: '', proxyUrl: '', costMultiplier: 1, overrides: {},
  name: '',
  baseUrl: '',
  apiKey: '',
  authType: 'bearer',
  headerProfile: 'generic',
  appendBetaQuery: false,
  headerOverridesText: '',
  wireProfile: 'default',
  tlsProfile: 'default',
  enabled: false,
  priority: 100,
  maxConcurrentRequests: 10,
  usageProjectionMode: 'pass_through',
  streamResponseMode: 'inherit',
  preOutputStreamRetryMode: 'inherit',
  autoDisablePolicy: 'inherit',
  normalizeModelVersionDots: false,
  supportedModelsText: '',
  routeMode: 'allow_all',
  routeRules: [],
  modelMappingMode: DEFAULT_POOL_MODEL_MAPPING_MODE,
  modelMappingRequireMatch: false,
  modelMappingRulesText: '',
  notes: '',
})

export const accountFormFromAccount = (account: Account): AccountFormDraft => ({
 tagsText: (account.tags??[]).join(', '), proxyUrl: account.proxyUrl??'', costMultiplier: account.costMultiplier??1, overrides: {...account.overrides},
  name: account.name,
  baseUrl: account.baseUrl,
  apiKey: '',
  authType: account.authType,
  headerProfile: account.headerProfile || 'generic',
  appendBetaQuery: Boolean(account.appendBetaQuery),
  headerOverridesText: joinHeaderOverrides(account.headerOverrides || {}),
  wireProfile: account.wireProfile || 'default',
  tlsProfile: account.tlsProfile || 'default',
  enabled: account.enabled,
  priority: account.priority,
  maxConcurrentRequests: account.maxConcurrentRequests,
  usageProjectionMode: account.usageProjectionMode,
  streamResponseMode: account.streamResponseMode || 'inherit',
  preOutputStreamRetryMode: account.preOutputStreamRetryMode || 'inherit',
  autoDisablePolicy: account.autoDisablePolicy,
  normalizeModelVersionDots: Boolean(account.normalizeModelVersionDots),
  supportedModelsText: joinRules(account.supportedModels || []),
  routeMode: account.routeMode || 'allow_all',
  routeRules: [...(account.routeRules || [])],
  modelMappingMode: account.modelMappingMode || DEFAULT_POOL_MODEL_MAPPING_MODE,
  modelMappingRequireMatch: Boolean(account.modelMappingRequireMatch),
  modelMappingRulesText: joinModelMappingRules(account.modelMappingRules || []),
  notes: account.notes || '',
})

export function modelMappingPresetsForMode(mode: AccountFormDraft['modelMappingMode']): AccountModelMappingPreset[] {
  if (mode === 'passthrough_mapping') return DIRECT_MODEL_MAPPING_PRESETS
  if (mode === 'direct_mapping') return DIRECT_MODEL_MAPPING_PRESETS
  if (mode === 'processed_mapping') return PROCESSED_MODEL_MAPPING_PRESETS
  return []
}

export function appendModelMappingRules(currentText: string, incomingRules: AccountModelMappingRule[]): { text: string; added: number } {
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

export function appendModelMappingPreset(currentText: string, preset: AccountModelMappingPreset): { text: string; added: boolean } {
  const result = appendModelMappingRules(currentText, [{ enabled: true, source: preset.source, target: preset.target, kind: 'alias' }])
  return { text: result.text, added: result.added > 0 }
}

export function appendModelMappingPresets(currentText: string, presets: AccountModelMappingPreset[]): { text: string; added: number } {
  return appendModelMappingRules(currentText, presets.map((p) => ({ enabled: true, source: p.source, target: p.target, kind: 'alias' })))
}

export function modelMappingDescription(mode: Account['modelMappingMode'] | undefined, normalizeFallback: boolean): string {
  const processedFallback = normalizeFallback ? '未命中后使用内部处理模型，并把数字点号转横杠。' : '未命中后使用内部处理模型。'
  if (mode === 'passthrough') return '直接使用客户端请求里的模型，不应用映射规则和兜底转换。'
  if (mode === 'passthrough_mapping') return '先用客户端请求模型匹配规则；未命中时仍使用原请求模型。'
  if (mode === 'direct_mapping') return `用客户端请求模型匹配规则；${processedFallback}`
  return `先使用本系统解析后的模型匹配规则；${processedFallback}`
}

export function accountModelMappingSummary(account: Account): string {
  if (account.modelMappingMode === 'passthrough') return '原样'
  const count = account.modelMappingRules?.length || 0
  const mode = account.modelMappingMode === 'passthrough_mapping'
    ? '原样+映射'
    : account.modelMappingMode === 'direct_mapping'
      ? '映射+内部'
      : '内部+映射'
  const fallback = account.modelMappingRequireMatch ? '必须命中' : account.normalizeModelVersionDots ? '未命中4.8->4-8' : '允许未命中'
  return `${mode}${count ? ` ${count}条` : ''} · ${fallback}`
}

export function headerProfileDescription(profile: Account['headerProfile'] | undefined): string {
  if (profile === 'claude_code_mimic') {
    return '忽略客户端指纹，使用 Claude Code 风格的 x-stainless、User-Agent、x-client-request-id 和 anthropic-beta；URL 自动追加 beta=true。'
  }
  if (profile === 'anthropic_passthrough') {
    return '只透传 Anthropic/Claude Code 相关请求头，过滤 cookie、x-goog-api-key 和非必要客户端头；URL 自动追加 beta=true。'
  }
  return '保持旧版泛用转发策略，仅过滤认证、连接、cookie、x-goog-api-key 等敏感请求头；是否追加 beta=true 由下方开关控制。'
}

export function accountHeaderProfileSummary(account: Account): string {
  const profile = account.headerProfile || 'generic'
  const overrides = Object.keys(account.headerOverrides || {}).length
  const parts = [
    profile === 'claude_code_mimic'
      ? 'Header：Claude Code'
      : profile === 'anthropic_passthrough'
        ? 'Header：Anthropic'
        : 'Header：默认',
  ]
  if (account.appendBetaQuery || profile !== 'generic') parts.push('beta=true')
  if (overrides > 0) parts.push(`${overrides} 个覆盖`)
  if (account.wireProfile === 'http1_title_case') parts.push('HTTP/1')
  if (account.tlsProfile === 'native_tls') parts.push('Native TLS')
  return parts.join(' · ')
}

export function wireProfileDescription(profile: Account['wireProfile'] | undefined): string {
  if (profile === 'http1_title_case') {
    return '强制 HTTP/1，并启用 reqwest 的 HTTP/1 Title-Case header 写出；这是兼容增强，不是完整 Claude Code 混合大小写/顺序复刻。'
  }
  return '使用默认 reqwest 传输行为，允许 HTTP/2 协商，header wire 大小写由 HTTP 客户端决定。'
}

export function tlsProfileDescription(profile: Account['tlsProfile'] | undefined): string {
  if (profile === 'native_tls') {
    return '使用系统/native-tls 栈构建账号客户端；这是 TLS 栈切换，不是完整 Node.js/Claude Code ClientHello 指纹。'
  }
  return '使用默认 TLS 栈。当前项目默认仍是常规 reqwest/rustls/native 配置，不伪造 Node.js ClientHello。'
}

export function accountSupportedModelsSummary(account: Account): string {
  const models = account.supportedModels || []
  if (models.length === 0) return '支持：不限制'
  if (models.length <= 2) return `支持：${models.join(', ')}`
  return `支持：${models[0]}, ${models[1]} 等 ${models.length} 个`
}

export function accountRouteSummary(account: Account): string {
  const rules = account.routeRules || []
  if (!account.routeMode || account.routeMode === 'allow_all') return '入口：不限制'
  if (account.routeMode === 'allow_list') {
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

export function usageProjectionDescription(mode: Account['usageProjectionMode'] | undefined): string {
  if (mode === 'current_path_policy') {
    return '返回给下游的 usage 按当前入口路径的缓存策略整理；该路径是 no-cache、非流式缓存选了强制不缓存，或路径开了官方条件严格检查而请求不满足时，保持上游原始 usage。'
  }
  return '返回给下游的 usage 保持账号上游原始值；不按当前入口路径整理。'
}

export function streamResponseDescription(mode: AccountStreamResponseDraft): string {
  if (mode === 'event_passthrough') {
    return '仅控制 stream=true 的 SSE 事件转发方式。文本、thinking、tool 等普通事件按上游事件级转发；usage 是否透传上游或按路径整理，由上面的下游 usage 口径决定。'
  }
  return '当前账号不单独指定流式 SSE 转发方式，使用账号全局默认值；usage 口径仍由上面的下游 usage 口径决定。'
}

export function streamRetryDescription(mode: AccountStreamRetryDraft): string {
  if (mode === 'enabled') {
    return '当前账号强制启用：stream 在 message_start、ping 等协议前缀后遇到 error、断流、EOF 或空闲超时时，可在未提交内容前换其他账号。'
  }
  if (mode === 'disabled') {
    return '当前账号强制关闭：stream body 内错误保持现有流错误行为，不把本次请求重放到其他账号。'
  }
  if (mode === 'early_commit') {
    return '收到上游 message_start 后立即转发（usage 置 0）并持续发送 ping；首个内容前出错仍可换账号重试，下游只收到一次 message_start。此模式取消首个输出超时，保留空闲超时。首包前全部失败仍返回 HTTP 错误；首包后全部失败发送 SSE error，用量保持 0。成功时在 message_delta 返回完整用量。仅当本次请求所有可选账号都设为此模式时生效。'
  }
  return '当前账号继承全局“流式首输出前错误换池”开关。'
}

export function streamRetrySummary(account: Account, config: AccountsConfig): string {
  const mode = account.preOutputStreamRetryMode || 'inherit'
  if (mode === 'enabled') return '首输出恢复：启用'
  if (mode === 'disabled') return '首输出恢复：禁用'
  if (mode === 'early_commit') return '首输出恢复：先提交首包'
  return config.streamPreOutputRetryEnabled
    ? '首输出恢复：继承启用'
    : '首输出恢复：继承禁用'
}

export function accountUsageSummary(account: Account, config: AccountsConfig): string {
  const parts = account.usageProjectionMode === 'current_path_policy'
    ? ['Usage：按路径整理']
    : ['Usage：透传上游']
  const streamMode = account.streamResponseMode || config.streamResponseMode
  parts.push(streamMode === 'event_passthrough' ? '流式：事件透传' : '流式：继承默认')
  if (account.streamResponseMode) {
    parts.push('单池覆盖')
  }
  if (account.usageProjectionMode !== 'current_path_policy') {
    return parts.join(' · ')
  }
  if (config.usageProjectionUpliftPercent > 0) {
    parts.push(`缓存 +${config.usageProjectionUpliftPercent}%`)
  }
  if (config.usageProjectionCostFloorEnabled) {
    parts.push(`成本底线 +${config.usageProjectionCostFloorMarginPercent}%`)
  }
  if (config.usageProjectionOutputUpliftMinTokens > 0 && config.usageProjectionOutputUpliftPercent > 0) {
    parts.push(`输出 >= ${config.usageProjectionOutputUpliftMinTokens} 后 +${config.usageProjectionOutputUpliftPercent}%`)
  }
  return parts.join(' · ')
}

export function authLabel(authType: Account['authType']): string {
  return authType === 'x_api_key' ? 'x-api-key' : 'Bearer'
}

export function modelMappingPresetClass(tone: AccountModelMappingPreset['tone']): string {
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
