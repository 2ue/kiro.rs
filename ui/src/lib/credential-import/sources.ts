// 识别导入内容的来源，并把各来源的账号结构转成 AddCredentialRequest。
//
// 支持：本项目导出（kiro-rs）、Kiro Account Manager（KAM）、kiro-manager-lite、Kiro-Go、
// 通用 JSON / JSONL、纯文本 API Key（每行 `ksk_...`，可带 `|region` 或 `----region`），
// 以及 KAM / lite 的文本导出（卡密、CSV、TXT）。

import type { AddCredentialRequest } from '@/types/api'
import { parseAccountTextRows } from './text-formats'
import {
  asBoolean,
  asCount,
  asString,
  asStringList,
  asTimestamp,
  isObject,
  pick,
  pickObject,
  type JsonObject,
} from './values'

export type ImportSource =
  | 'auto'
  | 'kiro-rs'
  | 'kam'
  | 'kiro-manager-lite'
  | 'kiro-go'
  | 'account-text'
  | 'api-key-text'
  | 'generic'
export type DetectedSource = Exclude<ImportSource, 'auto'>

export const IMPORT_SOURCE_OPTIONS: Array<{ value: ImportSource; label: string }> = [
  { value: 'auto', label: '自动识别' },
  { value: 'kiro-rs', label: '本项目导出' },
  { value: 'kam', label: 'KAM / Kiro-Go 导出包' },
  { value: 'kiro-manager-lite', label: 'kiro-manager-lite' },
  { value: 'kiro-go', label: 'Kiro-Go 配置' },
  { value: 'account-text', label: 'KAM / lite 文本（卡密 / CSV / TXT）' },
  { value: 'api-key-text', label: 'API Key 文本' },
  { value: 'generic', label: '通用 JSON' },
]

export function importSourceLabel(source: ImportSource): string {
  return IMPORT_SOURCE_OPTIONS.find((option) => option.value === source)?.label ?? source
}

/** 一条解析出的账号：请求体，以及来源文件里能看出的附加信息。 */
export interface ParsedImportAccount {
  credential: AddCredentialRequest
  /** 来源工具记录的账号状态（如 error / banned / expired），只用于提示和可选跳过。 */
  sourceStatus?: string
  /** 来源文件里账号是否标记为停用；只有用户选择导入禁用状态时才写进请求。 */
  sourceDisabled?: boolean
  /** 来源文件里的代理（URL 或本地代理资源 ID）；只有用户选择导入代理时才写进请求。 */
  sourceProxy?: Pick<AddCredentialRequest, 'proxyUrl' | 'proxyUsername' | 'proxyPassword' | 'proxyResourceId'>
}

export interface ImportParseResult {
  /** 按哪种来源读取：手动指定的来源，或自动识别的结果。 */
  source: DetectedSource
  /** 按内容结构识别出的来源；手动指定时可以和 source 对比。 */
  detected: DetectedSource
  accounts: ParsedImportAccount[]
  /** 解析时跳过的条目及原因（缺少 refreshToken 等）。 */
  skipped: string[]
}

type AuthMethod = NonNullable<AddCredentialRequest['authMethod']>

// ----------------------------------------------------------------------------
// 认证类型与 provider
// ----------------------------------------------------------------------------

function compact(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]/g, '')
}

function authMethodFrom(value: unknown): AuthMethod | undefined {
  const text = asString(value)
  if (!text) return undefined
  const key = compact(text)
  if (key === 'social') return 'social'
  if (key === 'idc' || key === 'builderid' || key === 'iam') return 'idc'
  if (key === 'apikey') return 'api_key'
  if (key === 'externalidp' || key === 'enterprise' || key === 'iamsso' || key === 'awsidc' || key === 'internal') return 'external_idp'
  return undefined
}

/** provider 统一写法：Google / Github / BuilderId / Enterprise，其它值原样保留。 */
function providerFrom(value: unknown): string | undefined {
  const text = asString(value)
  if (!text) return undefined
  const key = compact(text)
  if (key === 'google' || key === 'gmail') return 'Google'
  if (key === 'github' || key === 'gh') return 'Github'
  if (key === 'builderid') return 'BuilderId'
  if (key === 'enterprise' || key === 'awsidc' || key === 'iamsso' || key === 'internal') return 'Enterprise'
  return text
}

/**
 * 认证类型：
 * - 有 tokenEndpoint（外部 IdP 自己的 token 地址）→ external_idp
 * - 显式 authMethod 优先，但标成 Enterprise 系又带 clientId + clientSecret 的按 idc
 * - 有 clientId + clientSecret → idc（Builder ID 与组织 IdC 都是 AWS OIDC）
 * - 只有 refreshToken → social
 */
function resolveAuthMethod(input: {
  authMethod?: AuthMethod
  clientId?: string
  clientSecret?: string
  tokenEndpoint?: string
  kiroApiKey?: string
}): AuthMethod {
  if (input.kiroApiKey) return 'api_key'
  if (input.tokenEndpoint) return 'external_idp'
  // KAM / lite / Kiro-Go 的 Enterprise 是 AWS IAM Identity Center 组织账号：带 clientId + clientSecret，
  // 走 AWS OIDC 刷新，在本项目里属于 idc。只有带 tokenEndpoint 的才是外部 IdP。
  if (input.authMethod === 'external_idp' && input.clientId && input.clientSecret) return 'idc'
  if (input.authMethod) return input.authMethod
  if (input.clientId && input.clientSecret) return 'idc'
  return 'social'
}

// ----------------------------------------------------------------------------
// API Key
// ----------------------------------------------------------------------------

/** `ksk_xxx`、`ksk_xxx|us-east-1`、`ksk_xxx----us-east-1`。 */
function splitApiKey(value: string): { key: string; region?: string } {
  const [rawKey, rawRegion] = value.trim().split(/\||----/, 2)
  return { key: rawKey?.trim() ?? '', region: rawRegion?.trim() || undefined }
}

function apiKeyCredential(rawKey: string, regionHint?: string): AddCredentialRequest | null {
  const { key, region: inlineRegion } = splitApiKey(rawKey)
  if (!key.startsWith('ksk_')) return null
  const region = inlineRegion ?? regionHint
  return { authMethod: 'api_key', kiroApiKey: key, region, authRegion: region, apiRegion: region, endpoint: 'cli' }
}

/** 每行一个 API Key（`#` 开头为注释）。任何一行不是 API Key 时返回 null。 */
function parseApiKeyLines(text: string): ParsedImportAccount[] | null {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter((line) => line && !line.startsWith('#'))
  if (!lines.length) return null
  const accounts: ParsedImportAccount[] = []
  for (const line of lines) {
    const credential = apiKeyCredential(line)
    if (!credential) return null
    accounts.push({ credential })
  }
  return accounts
}

// ----------------------------------------------------------------------------
// 标签 / 分组名与账号状态
// ----------------------------------------------------------------------------

/** KAM / lite 的账号里 tags、groupId(s) 存的是 id，名字在文件顶层的 tags / groups 里。 */
type NameLookup = Map<string, string>

function buildNameLookup(root: unknown): NameLookup {
  const lookup: NameLookup = new Map()
  if (!isObject(root)) return lookup
  const containers = [root, pickObject(pickObject(root, 'sections'), 'accounts')]
  for (const container of containers) {
    for (const key of ['tags', 'groups']) {
      const list = pick(container, key)
      if (!Array.isArray(list)) continue
      for (const entry of list) {
        if (!isObject(entry)) continue
        const id = asString(pick(entry, 'id'))
        const name = asString(pick(entry, 'name'))
        if (id && name) lookup.set(id, name)
      }
    }
  }
  return lookup
}

function tagNames(item: JsonObject, lookup: NameLookup): string[] {
  const ids = [
    ...asStringList(pick(item, 'tags', 'labels')),
    ...asStringList(pick(item, 'groupIds')),
    ...asStringList(pick(item, 'groupId')),
  ]
  const names = ids.map((id) => lookup.get(id) ?? (lookup.size ? undefined : id)).filter((name): name is string => Boolean(name))
  return Array.from(new Set(names))
}

/** 被来源工具标为封禁的错误文本（KAM 只在 lastError 里体现封禁）。 */
const BANNED_ERROR_PATTERN = /accountsuspended|temporarily_suspended|\b423\b|banned|suspended|已封禁/i

/**
 * 来源工具记录的账号状态，统一成小写：
 * - lite：status 有 banned；KAM：status 只有 error，封禁写在 lastError
 * - Kiro-Go：banStatus 为 BANNED / SUSPENDED / DISABLED，ACTIVE 视为正常
 * - lite 的回收站：trashedAt 存在
 */
function accountStatus(item: JsonObject): string | undefined {
  const banStatus = asString(pick(item, 'banStatus'))?.toLowerCase()
  if (banStatus && banStatus !== 'active') return banStatus
  if (pick(item, 'trashedAt') !== undefined) return 'trashed'
  const lastError = asString(pick(item, 'lastError'))
  if (lastError && BANNED_ERROR_PATTERN.test(lastError)) return 'banned'
  const status = asString(pick(item, 'status'))?.toLowerCase()
  return status && status !== 'active' ? status : undefined
}

// ----------------------------------------------------------------------------
// 通用账号字段提取（所有 JSON 来源共用）
// ----------------------------------------------------------------------------

function profileArnRegion(profileArn: string | undefined): string | undefined {
  const parts = profileArn?.split(':') ?? []
  return parts.length >= 6 && parts[0] === 'arn' && parts[2] === 'codewhisperer' ? parts[3] || undefined : undefined
}

/**
 * 从一个账号对象取字段。凭据字段可能在顶层，也可能在 `credentials` 子对象里；
 * 子对象优先（KAM / Lite / Kiro-Go 的 token 都在 credentials 里，顶层同名字段是摘要）。
 */
function readAccount(item: JsonObject, lookup: NameLookup): ParsedImportAccount | string {
  const nested = pickObject(item, 'credentials', 'credential', 'auth', 'token', 'tokens')
  // Lite 的精简导出可能把原始 kiro-auth-token.json 整块嵌在 kiro_auth_token_raw 里
  const rawToken = pickObject(item, 'kiroAuthTokenRaw')
  const field = (...names: string[]) => pick(nested, ...names) ?? pick(rawToken, ...names) ?? pick(item, ...names)

  const rawApiKey = asString(field('kiroApiKey', 'apiKey', 'key'))
  const regionField = asString(field('region'))
  if (rawApiKey && splitApiKey(rawApiKey).key.startsWith('ksk_')) {
    const credential = apiKeyCredential(rawApiKey, regionField)
    if (credential) {
      credential.email = asString(field('email'))
      credential.tags = tagNames(item, lookup)
      return finishAccount(item, field, credential)
    }
  }

  const refreshToken = asString(field('refreshToken'))
  if (!refreshToken) return '缺少 refreshToken'
  // 与服务端 validate_refresh_token 相同的规则，提前在预览里排除
  if (refreshToken.length < 100 || refreshToken.includes('...')) return 'refreshToken 不完整（不足 100 个字符或含 ...），服务端会拒绝'

  const clientId = asString(field('clientId'))
  const clientSecret = asString(field('clientSecret'))
  const tokenEndpoint = asString(field('tokenEndpoint'))
  // KAM 刷新账号时用 Portal GetUserInfo 的结果更新顶层 idp，不改 credentials.provider，所以顶层优先
  const provider = providerFrom(pick(item, 'idp') ?? field('provider', 'idp', 'loginProvider', 'loginOption'))
  const authMethod = resolveAuthMethod({
    authMethod: authMethodFrom(field('authMethod', 'authType')),
    clientId,
    clientSecret,
    tokenEndpoint,
  })
  const profileArn = asString(field('profileArn'))

  const credential: AddCredentialRequest = {
    authMethod,
    provider,
    refreshToken,
    accessToken: asString(field('accessToken')),
    expiresAt: asTimestamp(field('expiresAt', 'expired', 'expiry', 'expiration')),
    clientId: authMethod === 'social' ? undefined : clientId,
    clientSecret: authMethod === 'idc' ? clientSecret : undefined,
    tokenEndpoint: authMethod === 'external_idp' ? tokenEndpoint : undefined,
    issuerUrl: authMethod === 'external_idp' ? asString(field('issuerUrl')) : undefined,
    scopes: authMethod === 'external_idp' ? asString(field('scopes', 'scope')) : undefined,
    profileArn,
    email: asString(field('email', 'loginHint')),
    region: regionField,
    authRegion: asString(field('authRegion')),
    apiRegion: asString(field('apiRegion')) ?? profileArnRegion(profileArn),
    machineId: asString(field('machineId')),
    tags: tagNames(item, lookup),
  }
  return finishAccount(item, field, credential)
}

/** 本项目自己的调度参数（其它工具没有这些字段），以及状态 / 代理。 */
function finishAccount(
  item: JsonObject,
  field: (...names: string[]) => unknown,
  credential: AddCredentialRequest,
): ParsedImportAccount {
  credential.priority = asCount(field('priority'))
  credential.fast = asCount(field('fast'))
  credential.maxConcurrentRequests = asCount(field('maxConcurrentRequests', 'concurrency'))
  credential.rpm = asCount(field('rpm'))
  credential.endpoint = asString(field('endpoint')) ?? credential.endpoint
  credential.enableOverageAfterImport = asBoolean(field('enableOverageAfterImport'))

  const sourceStatus = accountStatus(item)
  const disabledFlag = asBoolean(pick(item, 'disabled'))
  const enabledFlag = asBoolean(pick(item, 'enabled'))
  const sourceDisabled = disabledFlag ?? (enabledFlag === undefined ? undefined : !enabledFlag)

  const proxyUrl = asString(field('proxyUrl', 'proxy'))
  const proxyResourceId = asCount(field('proxyResourceId'))
  const sourceProxy = proxyUrl || proxyResourceId !== undefined
    ? {
        proxyUrl,
        proxyUsername: asString(field('proxyUsername')),
        proxyPassword: asString(field('proxyPassword')),
        proxyResourceId,
      }
    : undefined

  return { credential, sourceStatus, sourceDisabled, sourceProxy }
}

// ----------------------------------------------------------------------------
// 来源识别
// ----------------------------------------------------------------------------

function parseJsonOrJsonl(text: string): unknown[] {
  try {
    return [JSON.parse(text)]
  } catch (jsonError) {
    const lines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean)
    if (lines.length <= 1) throw jsonError
    return lines.map((line, index) => {
      try {
        return JSON.parse(line)
      } catch (lineError) {
        throw new Error(`第 ${index + 1} 行不是合法 JSON：${lineError instanceof Error ? lineError.message : String(lineError)}`)
      }
    })
  }
}

/**
 * 按文件结构识别来源。
 * - 本项目导出：裸数组，或 `format: "kiro-rs-credentials-backup"` 包装
 * - kiro-manager-lite：完整备份 `format: "kiro-manager-lite-data"`，或账号导出 `app: "kiro-account-lite"`
 * - KAM 导出包 `{version, exportedAt, accounts:[{idp, credentials:{...}}]}`；Kiro-Go 的导出接口
 *   产出同样的结构，按同一种读取
 * - Kiro-Go 配置：`accounts` 里是扁平账号（顶层 refreshToken，带 enabled / proxyURL）
 */
function detectJsonSource(root: unknown): DetectedSource {
  if (Array.isArray(root)) {
    // 本项目的 json 导出是裸数组；它总是写出 disabled，authMethod 是规范值
    const first = root.find(isObject)
    if (first && typeof pick(first, 'disabled') === 'boolean' && ['social', 'idc', 'external_idp', 'api_key'].includes(asString(pick(first, 'authMethod')) ?? '')) {
      return 'kiro-rs'
    }
    return 'generic'
  }
  if (!isObject(root)) return 'generic'
  if (asString(pick(root, 'type')) === 'kiro-account-manager-config' || asString(pick(root, 'app')) === 'kiro-account-manager') {
    throw new Error('这是 Kiro Account Manager 的配置导出，不包含账号凭据；请在 KAM 的账号页导出 JSON')
  }
  const format = asString(pick(root, 'format'))
  if (format === 'kiro-rs-credentials-backup') return 'kiro-rs'
  if (format === 'kiro-manager-lite-data' || asString(pick(root, 'app')) === 'kiro-account-lite') return 'kiro-manager-lite'
  const accounts = pick(root, 'accounts')
  if (!Array.isArray(accounts)) return 'generic'
  const first = accounts.find(isObject)
  if (!first) return 'generic'
  if (pickObject(first, 'credentials')) return 'kam'
  if (pick(first, 'refreshToken') !== undefined && (pick(first, 'enabled') !== undefined || pick(first, 'proxyUrl') !== undefined)) return 'kiro-go'
  return 'generic'
}

/** 取出账号数组；不同来源的包装层级不同。 */
function accountItems(root: unknown): unknown[] {
  if (Array.isArray(root)) return root.flatMap(accountItems)
  if (!isObject(root)) return []
  const sections = pickObject(root, 'sections')
  if (sections) {
    // lite 整库备份：账号在 sections.accounts.accounts，Kiro API Key 在 sections.apiKeys.keys；
    // sections.proxyApiKeys 是 lite 自己发给客户端的 sk- 密钥，不是 Kiro 凭据
    const sectionAccounts = pick(pickObject(sections, 'accounts'), 'accounts')
    const sectionKeys = pick(pickObject(sections, 'apiKeys'), 'keys')
    return [
      ...(Array.isArray(sectionAccounts) ? sectionAccounts : []),
      ...(Array.isArray(sectionKeys) ? sectionKeys : []),
    ].flatMap(accountItems)
  }
  for (const key of ['credentials', 'accounts']) {
    const value = pick(root, key)
    if (Array.isArray(value)) return value.flatMap(accountItems)
  }
  const data = pickObject(root, 'data')
  if (data) {
    const nested = pick(data, 'credentials', 'accounts')
    if (Array.isArray(nested)) return nested.flatMap(accountItems)
  }
  return [root]
}

// ----------------------------------------------------------------------------
// 入口
// ----------------------------------------------------------------------------

function collectAccounts(
  source: DetectedSource,
  detected: DetectedSource,
  items: Array<{ label: string; item: unknown }>,
  lookup: NameLookup,
): ImportParseResult {
  const accounts: ParsedImportAccount[] = []
  const skipped: string[] = []
  for (const { label, item } of items) {
    if (!isObject(item)) {
      skipped.push(`${label}：不是对象`)
      continue
    }
    const parsed = readAccount(item, lookup)
    if (typeof parsed === 'string') skipped.push(`${asString(pick(item, 'email')) ?? label}：${parsed}`)
    else accounts.push(parsed)
  }
  return { source, detected, accounts, skipped }
}

/**
 * 解析一段导入内容。`source` 为 auto 时按结构识别；手动指定 JSON 来源时仍按同一套宽松规则读取账号，
 * 指定文本来源（API Key / 账号文本）时只按该文本格式解析。
 */
export function parseImportText(text: string, source: ImportSource = 'auto'): ImportParseResult {
  const trimmed = text.replace(/^\uFEFF/, '').trim()
  if (!trimmed) {
    const empty = source === 'auto' ? 'generic' : source
    return { source: empty, detected: empty, accounts: [], skipped: [] }
  }

  const apiKeys = parseApiKeyLines(trimmed)
  if (apiKeys && (source === 'auto' || source === 'api-key-text')) {
    return { source: 'api-key-text', detected: 'api-key-text', accounts: apiKeys, skipped: [] }
  }
  if (source === 'api-key-text') throw new Error('每行应为一个以 ksk_ 开头的 API Key（可带 |region 或 ----region）')

  let roots: unknown[]
  try {
    roots = parseJsonOrJsonl(trimmed)
  } catch (jsonError) {
    if (source !== 'auto' && source !== 'account-text') throw jsonError
    const rows = parseAccountTextRows(trimmed)
    if (!rows) {
      const textFormats = '卡密（---- 分隔）、带表头的 CSV、email,refreshToken、每行一个 refreshToken 或 ksk_ API Key'
      throw new Error(source === 'account-text'
        ? `无法识别的文本格式，支持：${textFormats}`
        : `无法识别：不是合法的 JSON / JSONL（${jsonError instanceof Error ? jsonError.message : String(jsonError)}），也不是${textFormats}`)
    }
    const items = rows.map((row) => ({ label: `第 ${row.line} 行`, item: row.account }))
    return collectAccounts('account-text', 'account-text', items, new Map())
  }
  if (source === 'account-text') throw new Error('内容是 JSON，请选择对应的 JSON 来源或自动识别')

  // JSONL 的每一行都是一个账号，按数组识别
  const detected = detectJsonSource(roots.length > 1 ? roots : roots[0])
  const chosen = source === 'auto' ? detected : source
  const items = roots.flatMap(accountItems).map((item, index) => ({ label: `第 ${index + 1} 条`, item }))
  return collectAccounts(chosen, detected, items, buildNameLookup(roots[0]))
}

/** 逐个读取文件内容；每个文件单独解析，避免多个 JSON 文件拼接后无法解析。 */
export async function readImportFiles(files: File[]): Promise<{ inputs: Array<{ name: string; text: string }>; errors: string[] }> {
  const inputs: Array<{ name: string; text: string }> = []
  const errors: string[] = []
  for (const file of files) {
    try {
      inputs.push({ name: file.name, text: await file.text() })
    } catch (error) {
      errors.push(`${file.name}: ${error instanceof Error ? error.message : String(error)}`)
    }
  }
  return { inputs, errors }
}

export interface ParsedImportInput {
  name: string
  result?: ImportParseResult
  error?: string
}

/** 每段内容（粘贴的文本或一个文件）单独解析，一段失败不影响其它。 */
export function parseImportInputs(inputs: Array<{ name: string; text: string }>, source: ImportSource = 'auto'): ParsedImportInput[] {
  return inputs.map(({ name, text }) => {
    try {
      return { name, result: parseImportText(text, source) }
    } catch (error) {
      return { name, error: error instanceof Error ? error.message : String(error) }
    }
  })
}
