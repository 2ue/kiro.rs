import type { CreateExternalPoolRequest, ExternalPool, ExternalPoolModelMappingRule, UpdateExternalPoolRequest } from '@/api/types'

export type PoolDraft = Required<
  Pick<
    CreateExternalPoolRequest,
    | 'name'
    | 'baseUrl'
    | 'apiKey'
    | 'authType'
    | 'headerProfile'
    | 'appendBetaQuery'
    | 'wireProfile'
    | 'tlsProfile'
    | 'enabled'
    | 'priority'
    | 'maxConcurrentRequests'
    | 'requestBodyMode'
    | 'rawModelMode'
    | 'usageProjectionMode'
    | 'preOutputStreamRetryMode'
    | 'autoDisablePolicy'
    | 'preservePath'
    | 'normalizeModelVersionDots'
    | 'routeMode'
    | 'modelMappingMode'
    | 'modelMappingRequireMatch'
    | 'notes'
  >
> & {
  streamResponseMode: 'inherit' | 'event_passthrough'
  headerOverridesText: string
  supportedModels: string[]
  routeRules: string[]
  modelMappingText: string
}

export function emptyPoolDraft(): PoolDraft {
  return {
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
    requestBodyMode: 'normalized',
    rawModelMode: 'none',
    usageProjectionMode: 'pass_through',
    streamResponseMode: 'inherit',
    preOutputStreamRetryMode: 'inherit',
    autoDisablePolicy: 'inherit',
    preservePath: true,
    normalizeModelVersionDots: false,
    supportedModels: [],
    routeMode: 'allow_all',
    routeRules: [],
    modelMappingMode: 'processed_mapping',
    modelMappingRequireMatch: false,
    modelMappingText: '',
    notes: '',
  }
}

export function draftFromPool(p: ExternalPool): PoolDraft {
  return {
    name: p.name,
    baseUrl: p.baseUrl,
    apiKey: '',
    authType: p.authType,
    headerProfile: p.headerProfile || 'generic',
    appendBetaQuery: !!p.appendBetaQuery,
    headerOverridesText: Object.entries(p.headerOverrides ?? {})
      .map(([k, v]) => `${k}: ${v}`)
      .join('\n'),
    wireProfile: p.wireProfile || 'default',
    tlsProfile: p.tlsProfile || 'default',
    enabled: p.enabled,
    priority: p.priority,
    maxConcurrentRequests: p.maxConcurrentRequests,
    requestBodyMode: p.requestBodyMode || 'normalized',
    rawModelMode: p.rawModelMode || 'none',
    usageProjectionMode: p.usageProjectionMode,
    streamResponseMode: p.streamResponseMode ?? 'inherit',
    preOutputStreamRetryMode: p.preOutputStreamRetryMode || 'inherit',
    autoDisablePolicy: p.autoDisablePolicy,
    preservePath: p.preservePath !== false,
    normalizeModelVersionDots: !!p.normalizeModelVersionDots,
    supportedModels: p.supportedModels ?? [],
    routeMode: p.routeMode || 'allow_all',
    routeRules: p.routeRules ?? [],
    modelMappingMode: p.modelMappingMode || 'processed_mapping',
    modelMappingRequireMatch: !!p.modelMappingRequireMatch,
    modelMappingText: (p.modelMappingRules ?? []).map((r) => `${r.source} -> ${r.target}`).join('\n'),
    notes: p.notes ?? '',
  }
}

export function parseHeaderOverrides(text: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const raw of text.split('\n')) {
    const line = raw.trim()
    if (!line || line.startsWith('#')) continue
    const idx = line.indexOf(':') >= 0 ? line.indexOf(':') : line.indexOf('=')
    if (idx <= 0) continue
    const name = line.slice(0, idx).trim().toLowerCase()
    const value = line.slice(idx + 1).trim()
    if (name && value) out[name] = value
  }
  return out
}

export function parseMappingRules(text: string): ExternalPoolModelMappingRule[] {
  return text
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('#'))
    .map((l) => l.split(/\s*(?:->|=>|→|=)\s*/, 2))
    .map(([source, target]) => ({ enabled: true, source: source?.trim() ?? '', target: target?.trim() ?? '', kind: 'alias' as const }))
    .filter((r) => r.source && r.target)
}

export function draftToRequest(d: PoolDraft): UpdateExternalPoolRequest {
  return {
    name: d.name.trim(),
    baseUrl: d.baseUrl.trim(),
    apiKey: d.apiKey.trim() || undefined,
    authType: d.authType,
    headerProfile: d.headerProfile,
    appendBetaQuery: d.appendBetaQuery,
    headerOverrides: parseHeaderOverrides(d.headerOverridesText),
    wireProfile: d.wireProfile,
    tlsProfile: d.tlsProfile,
    enabled: d.enabled,
    priority: Math.max(0, Math.floor(d.priority)),
    maxConcurrentRequests: Math.max(1, Math.floor(d.maxConcurrentRequests)),
    requestBodyMode: d.requestBodyMode,
    rawModelMode: d.rawModelMode,
    usageProjectionMode: d.usageProjectionMode,
    streamResponseMode: d.streamResponseMode === 'inherit' ? null : d.streamResponseMode,
    preOutputStreamRetryMode: d.preOutputStreamRetryMode,
    autoDisablePolicy: d.autoDisablePolicy,
    preservePath: d.preservePath,
    normalizeModelVersionDots: d.normalizeModelVersionDots,
    supportedModels: d.supportedModels,
    routeMode: d.routeMode,
    routeRules: d.routeRules,
    modelMappingMode: d.modelMappingMode,
    modelMappingRequireMatch: d.modelMappingRequireMatch,
    modelMappingRules: parseMappingRules(d.modelMappingText),
    notes: d.notes,
  }
}

export const MAPPING_MODE_HINT: Record<PoolDraft['modelMappingMode'], string> = {
  passthrough: '直接使用客户端请求里的模型，不应用映射规则。',
  passthrough_mapping: '先用客户端请求模型匹配规则；未命中时使用原请求模型。',
  direct_mapping: '用客户端请求模型匹配规则；未命中时使用内部处理后的模型。',
  processed_mapping: '先按本系统解析后的模型匹配规则；未命中时使用内部处理后的模型。',
}

export const HEADER_PROFILE_HINT: Record<PoolDraft['headerProfile'], string> = {
  generic: '泛用转发：仅过滤认证、连接、cookie 等敏感请求头。',
  anthropic_passthrough: '只透传 Anthropic / Claude Code 相关请求头；URL 自动追加 beta=true。',
  claude_code_mimic: '使用 Claude Code 风格的请求头指纹；URL 自动追加 beta=true。',
}
