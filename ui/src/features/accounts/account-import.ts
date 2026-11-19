// 批量导入账号的纯逻辑：识别格式、解析、校验、去重、合并默认参数。
// 支持两种来源：JSON（数组 / 配置迁移包的 accounts / 单个对象 / 每行一个对象）和每行一条的文本
// `baseUrl apiKey [名称]`（先按空白切，认不出地址和密钥再试 | 和逗号分隔）。
import type { AccountAuthType, AccountOverrides, CreateAccountRequest, UsageProjectionMode } from '@/types/api'
import { normalizeTags } from '../../components/patterns/tag-utils.ts'

/** 创建账号接口认的字段；导出与导入都只保留这些。 */
export const ACCOUNT_REQUEST_FIELDS = [
  'name',
  'baseUrl',
  'apiKey',
  'authType',
  'enabled',
  'priority',
  'maxConcurrentRequests',
  'costMultiplier',
  'tags',
  'proxyUrl',
  'overrides',
  'headerProfile',
  'appendBetaQuery',
  'headerOverrides',
  'wireProfile',
  'tlsProfile',
  'usageProjectionMode',
  'streamResponseMode',
  'autoDisablePolicy',
  'preOutputStreamRetryMode',
  'normalizeModelVersionDots',
  'modelMappingMode',
  'modelMappingRequireMatch',
  'modelMappingRules',
  'supportedModels',
  'routeMode',
  'routeRules',
  'notes',
] as const satisfies ReadonlyArray<keyof CreateAccountRequest>

export const REDACTED = '<redacted>'

const OVERRIDE_FIELDS: Array<keyof AccountOverrides> = [
  'requestTimeoutSecs',
  'streamRequestTimeoutSecs',
  'streamIdleTimeoutSecs',
  'streamFirstOutputTimeoutSecs',
  'usageProjectionUpliftPercent',
  'usageProjectionCostFloorEnabled',
  'usageProjectionCostFloorMarginPercent',
  'usageProjectionOutputUpliftMinTokens',
  'usageProjectionOutputUpliftPercent',
]

export type ImportFormat = 'json' | 'jsonl' | 'config_package' | 'text' | 'empty'

export const IMPORT_FORMAT_LABELS: Record<ImportFormat, string> = {
  json: 'JSON 数组',
  jsonl: 'JSON 每行一个',
  config_package: '配置迁移包',
  text: '文本（每行一条）',
  empty: '空内容',
}

/** 从来源里读出的一条账号；error 表示这一条格式不对。 */
export interface ParsedImportEntry {
  /** 来源里的行号（文本 / JSONL）或数组下标 + 1。 */
  position: number
  account?: Partial<CreateAccountRequest> & { baseUrl: string; apiKey: string }
  error?: string
  warnings: string[]
}

export interface ParseImportResult {
  format: ImportFormat
  entries: ParsedImportEntry[]
  /** 整体无法解析时的原因（比如 JSON 语法错误）。 */
  error?: string
}

/** 与后端 mask_account_key 一致：去空白后不超过 10 个字符显示 ***，否则保留前 6 后 4。 */
export function maskApiKey(value: string): string {
  const trimmed = value.trim()
  if (trimmed.length <= 10) return '***'
  return `${trimmed.slice(0, 6)}...${trimmed.slice(-4)}`
}

/** 去重用的地址口径：去空白和末尾斜杠，协议和主机名不区分大小写（URL 解析时已转小写、去掉默认端口）。 */
export function normalizeBaseUrl(value: string): string {
  const trimmed = value.trim()
  try {
    const url = new URL(trimmed)
    return `${url.protocol}//${url.host}${url.pathname.replace(/\/+$/, '')}${url.search}`
  } catch {
    return trimmed.replace(/\/+$/, '')
  }
}

export function importDedupKey(baseUrl: string, apiKey: string): string {
  return `${normalizeBaseUrl(baseUrl)}\n${apiKey.trim()}`
}

/** 服务地址必须能被 URL 解析、协议是 http 或 https、且不含空白（含空白多半是把密钥或名称连进来了）。 */
export function isValidBaseUrl(value: string): boolean {
  const text = value.trim()
  if (!text || /\s/.test(text)) return false
  try {
    const url = new URL(text)
    return (url.protocol === 'http:' || url.protocol === 'https:') && url.hostname !== ''
  } catch {
    return false
  }
}

/**
 * 预览表“服务地址”列显示的值：只用通过校验的地址，地址里带的用户名、密码打码；
 * 不合法时显示 -，任何情况下都不把原始行或密钥原文放进这一列。
 */
export function displayBaseUrl(value: string): string {
  if (!isValidBaseUrl(value)) return '-'
  const text = value.trim()
  const url = new URL(text)
  if (!url.username && !url.password) return text
  if (url.username) url.username = '***'
  if (url.password) url.password = '***'
  return url.toString()
}

/** 文本里没有名称时的默认名：主机名 + 密钥后 4 位，便于区分同一服务的多个 Key。 */
export function defaultImportName(baseUrl: string, apiKey: string): string {
  let host = baseUrl.trim()
  try {
    host = new URL(baseUrl.trim()).host
  } catch {
    // 地址不合法时这条会被标为格式错误，名称只用于展示。
  }
  const key = apiKey.trim()
  return key.length > 4 ? `${host}-${key.slice(-4)}` : host
}

type ParsedTextLine = { baseUrl: string; apiKey: string; name?: string }
type TextLineAttempt = ParsedTextLine | { error: string; urlFound: boolean }

const NEED_URL_AND_KEY = '需要“地址 密钥 [名称]”三段，至少地址和密钥'
const NO_VALID_URL = '找不到合法的服务地址（需要 http:// 或 https:// 开头、能被解析且不含空白）'
/** 文本行的分隔符；地址段、密钥段里出现它们说明这一行的切法不对。 */
const SEPARATOR_CHARS = /[|,，]/
/** 依次尝试的切法：先空白，再 |，最后中英文逗号。 */
const LINE_SEPARATORS = [/\s+/, /\|/, /[,，]/]

/** 文本行里的地址段：除了是合法地址，还不能带分隔符（`https://a.com,` 是逗号格式切出来的半截）。 */
const isUrlSegment = (value: string) => !SEPARATOR_CHARS.test(value) && isValidBaseUrl(value)

/** 按分隔符切出前两段，剩下的原样作为名称（名称里可以有空格、逗号、|）。没有分隔符返回 null。 */
function splitFirstTwo(text: string, separator: RegExp): [string, string, string] | null {
  const re = new RegExp(separator.source, 'g')
  const first = re.exec(text)
  if (!first) return null
  const firstEnd = first.index + first[0].length
  const second = re.exec(text)
  if (!second) return [text.slice(0, first.index), text.slice(firstEnd), '']
  return [text.slice(0, first.index), text.slice(firstEnd, second.index), text.slice(second.index + second[0].length)]
}

function pickSegments(first: string, second: string, rest: string): TextLineAttempt {
  let url = first.trim()
  let key = second.trim()
  if (!isUrlSegment(url) && isUrlSegment(key)) [url, key] = [key, url]
  if (!isUrlSegment(url)) return { error: NO_VALID_URL, urlFound: false }
  if (!key) return { error: NEED_URL_AND_KEY, urlFound: true }
  if (/\s/.test(key)) return { error: '密钥里不能有空白', urlFound: true }
  if (SEPARATOR_CHARS.test(key)) return { error: '密钥里不能有 | 或逗号', urlFound: true }
  if (isValidBaseUrl(key)) return { error: '地址和密钥两段都是网址，找不到密钥', urlFound: true }
  return { baseUrl: url, apiKey: key, name: rest.trim() || undefined }
}

/**
 * 解析一行文本：`baseUrl apiKey [名称]`，地址和密钥顺序写反也能识别。
 * 先按空白切，能认出地址段和密钥段就按空白格式（名称里可以带逗号）；认不出再依次试 `|`、`,`/`，` 分隔。
 * 都认不出时整行标为格式错误，错误说明里不带原文。
 */
export function parseTextLine(line: string): ParsedTextLine | { error: string } {
  const trimmed = line.trim()
  const failures: Array<{ error: string; urlFound: boolean }> = []
  for (const separator of LINE_SEPARATORS) {
    const parts = splitFirstTwo(trimmed, separator)
    if (!parts) continue
    const result = pickSegments(...parts)
    if (!('error' in result)) return result
    failures.push(result)
  }
  // 优先给出“找到了地址但密钥不对”的具体原因；一种分隔都没有时提示至少两段。
  const specific = failures.find((failure) => failure.urlFound)
  if (specific) return { error: specific.error }
  return { error: failures.length === 0 ? NEED_URL_AND_KEY : NO_VALID_URL }
}

function camelKey(key: string): string {
  return key.replace(/[_-]([a-z0-9])/gi, (_, ch: string) => ch.toUpperCase())
}

/** 常见别名（不区分大小写）；规范字段名同时出现时以规范字段为准。 */
const KEY_ALIASES: Record<string, string> = {
  url: 'baseUrl',
  baseurl: 'baseUrl',
  base_url: 'baseUrl',
  key: 'apiKey',
  apikey: 'apiKey',
  api_key: 'apiKey',
  token: 'apiKey',
}

const toStringList = (value: unknown): string[] | undefined => {
  if (Array.isArray(value)) return value.filter((item): item is string => typeof item === 'string')
  if (typeof value === 'string') return value.split(/[,，;；\n]+/).map((item) => item.trim()).filter(Boolean)
  return undefined
}

const AUTH_TYPES: Record<string, AccountAuthType> = {
  bearer: 'bearer',
  authorization: 'bearer',
  x_api_key: 'x_api_key',
  'x-api-key': 'x_api_key',
  xapikey: 'x_api_key',
}

const USAGE_MODES: UsageProjectionMode[] = ['pass_through', 'current_path_policy']

/** 把一个 JSON 对象规整成创建账号请求；字段名接受 camelCase 和 snake_case，未知字段丢弃。 */
export function accountFromJsonObject(value: unknown, position: number): ParsedImportEntry {
  const warnings: string[] = []
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return { position, error: '不是 JSON 对象', warnings }
  }
  const source: Record<string, unknown> = {}
  for (const [rawKey, item] of Object.entries(value as Record<string, unknown>)) {
    const key = KEY_ALIASES[rawKey.toLowerCase()] ?? camelKey(rawKey)
    // 规范字段名总是生效；别名和 snake_case 只在规范字段没出现时补上。
    if (key === rawKey || !(key in source)) source[key] = item
  }
  const baseUrl = typeof source.baseUrl === 'string' ? source.baseUrl.trim() : ''
  const apiKey = typeof source.apiKey === 'string' ? source.apiKey.trim() : ''
  if (!baseUrl) return { position, error: '缺少 baseUrl', warnings }
  if (!isValidBaseUrl(baseUrl)) return { position, error: 'baseUrl 必须是 http:// 或 https:// 开头、能被解析且不含空白的地址', warnings }
  if (!apiKey) return { position, error: '缺少 apiKey', warnings }
  if (apiKey === REDACTED) return { position, error: '密钥已脱敏（导出时没有带密钥），不能导入', warnings }

  const account: Partial<CreateAccountRequest> & { baseUrl: string; apiKey: string } = { baseUrl, apiKey }
  for (const field of ACCOUNT_REQUEST_FIELDS) {
    if (field === 'baseUrl' || field === 'apiKey' || field === 'enabled') continue
    const item = source[field]
    if (item === undefined || item === null) continue
    ;(account as Record<string, unknown>)[field] = item
  }

  if (account.name !== undefined && typeof account.name !== 'string') delete account.name
  if (typeof account.name === 'string') account.name = account.name.trim() || undefined
  if (account.authType !== undefined) {
    const auth = AUTH_TYPES[String(account.authType).trim().toLowerCase()]
    if (!auth) return { position, error: `authType 只能是 bearer 或 x_api_key（读到 ${String(account.authType)}）`, warnings }
    account.authType = auth
  }
  if (account.usageProjectionMode !== undefined && !USAGE_MODES.includes(account.usageProjectionMode)) {
    return { position, error: `usageProjectionMode 只能是 pass_through 或 current_path_policy`, warnings }
  }
  for (const field of ['priority', 'maxConcurrentRequests', 'costMultiplier'] as const) {
    if (account[field] === undefined) continue
    const number = typeof account[field] === 'string' ? Number(account[field]) : account[field]
    if (typeof number !== 'number' || !Number.isFinite(number)) return { position, error: `${field} 不是数字`, warnings }
    account[field] = number
  }
  if (account.priority !== undefined && !Number.isInteger(account.priority)) return { position, error: 'priority 必须是整数', warnings }
  if (account.maxConcurrentRequests !== undefined && !(Number.isInteger(account.maxConcurrentRequests) && account.maxConcurrentRequests >= 1 && account.maxConcurrentRequests <= 10000)) {
    return { position, error: 'maxConcurrentRequests 必须是 1 到 10000 的整数', warnings }
  }
  if (account.costMultiplier !== undefined && !(account.costMultiplier > 0)) return { position, error: 'costMultiplier 必须大于 0', warnings }
  if (source.tags !== undefined) {
    const tags = toStringList(source.tags)
    if (tags) account.tags = normalizeTags(tags)
    else delete account.tags
  }
  if (source.supportedModels !== undefined) {
    const models = toStringList(source.supportedModels)
    if (models) account.supportedModels = models
    else delete account.supportedModels
  }
  if (source.routeRules !== undefined) {
    const rules = toStringList(source.routeRules)
    if (rules) account.routeRules = rules
    else delete account.routeRules
  }
  if (typeof account.proxyUrl === 'string') {
    const proxy = account.proxyUrl.trim()
    if (!proxy) delete account.proxyUrl
    else if (proxy.includes(REDACTED) || proxy.includes('***')) {
      delete account.proxyUrl
      warnings.push('代理地址里的凭据已脱敏，未导入代理')
    } else account.proxyUrl = proxy
  } else delete account.proxyUrl
  if (account.overrides !== undefined) {
    if (typeof account.overrides !== 'object' || Array.isArray(account.overrides)) delete account.overrides
    else {
      const overrides: Record<string, unknown> = {}
      for (const key of OVERRIDE_FIELDS) {
        const item = (account.overrides as Record<string, unknown>)[key]
        if (item !== undefined && item !== null) overrides[key] = item
      }
      account.overrides = overrides as AccountOverrides
    }
  }
  return { position, account, warnings }
}

function parseJsonValue(value: unknown): { format: ImportFormat; entries: ParsedImportEntry[]; error?: string } {
  if (Array.isArray(value)) {
    return { format: 'json', entries: value.map((item, index) => accountFromJsonObject(item, index + 1)) }
  }
  if (value && typeof value === 'object') {
    const accounts = (value as Record<string, unknown>).accounts
    if (Array.isArray(accounts)) {
      return { format: 'config_package', entries: accounts.map((item, index) => accountFromJsonObject(item, index + 1)) }
    }
    return { format: 'json', entries: [accountFromJsonObject(value, 1)] }
  }
  return { format: 'json', entries: [], error: 'JSON 顶层需要是数组、带 accounts 的对象或单个账号对象' }
}

/** 识别并解析一段导入内容。 */
export function parseImportText(text: string): ParseImportResult {
  const trimmed = text.trim()
  if (!trimmed) return { format: 'empty', entries: [] }
  if (trimmed.startsWith('[') || trimmed.startsWith('{')) {
    try {
      return parseJsonValue(JSON.parse(trimmed))
    } catch (error) {
      // 每行一个 JSON 对象（JSONL）
      const lines = trimmed.split(/\r?\n/)
      const objectLines = lines.map((line, index) => ({ line: line.trim(), number: index + 1 })).filter((item) => item.line)
      if (objectLines.length > 1 && objectLines.every((item) => item.line.startsWith('{'))) {
        return {
          format: 'jsonl',
          entries: objectLines.map((item) => {
            try {
              return accountFromJsonObject(JSON.parse(item.line), item.number)
            } catch {
              return { position: item.number, error: '这一行不是合法 JSON', warnings: [] }
            }
          }),
        }
      }
      return { format: 'json', entries: [], error: `JSON 解析失败：${error instanceof Error ? error.message : String(error)}` }
    }
  }
  const entries: ParsedImportEntry[] = []
  trimmed.split(/\r?\n/).forEach((rawLine, index) => {
    const line = rawLine.trim()
    if (!line || line.startsWith('#') || line.startsWith('//')) return
    const parsed = parseTextLine(line)
    if ('error' in parsed) entries.push({ position: index + 1, error: parsed.error, warnings: [] })
    else entries.push({ position: index + 1, account: { ...parsed, name: parsed.name }, warnings: [] })
  })
  return { format: 'text', entries }
}

// ---------------------------------------------------------------------------
// 默认参数与导入计划
// ---------------------------------------------------------------------------

/** 统一默认参数：数字留空表示用后端默认值；文件里写了的字段以文件为准，enabled 只看“导入后启用”。 */
export interface ImportDefaults {
  enabled: boolean
  priority: string
  maxConcurrentRequests: string
  costMultiplier: string
  tags: string[]
  supportedModels: string[]
  authType: AccountAuthType
  usageProjectionMode: UsageProjectionMode
}

export function initialImportDefaults(): ImportDefaults {
  return {
    enabled: false,
    priority: '',
    maxConcurrentRequests: '',
    costMultiplier: '',
    tags: [],
    supportedModels: [],
    authType: 'bearer',
    usageProjectionMode: 'current_path_policy',
  }
}

const optionalNumber = (value: string) => (value.trim() === '' ? undefined : Number(value.trim()))

export function validateImportDefaults(defaults: ImportDefaults): string | null {
  const priority = optionalNumber(defaults.priority)
  if (priority !== undefined && !Number.isInteger(priority)) return '默认优先级必须是整数'
  const concurrency = optionalNumber(defaults.maxConcurrentRequests)
  if (concurrency !== undefined && !(Number.isInteger(concurrency) && concurrency >= 1 && concurrency <= 10000)) {
    return '默认并发必须是 1 到 10000 的整数'
  }
  const multiplier = optionalNumber(defaults.costMultiplier)
  if (multiplier !== undefined && !(Number.isFinite(multiplier) && multiplier > 0)) return '默认成本倍率必须大于 0'
  return null
}

export type ImportRowState = 'new' | 'duplicate' | 'exists' | 'invalid'

export interface ImportPlanRow {
  /** 来源名（文件名或“粘贴内容”）。 */
  origin: string
  position: number
  name: string
  /** 只用于预览展示（见 displayBaseUrl）；真正提交的地址在 request 里。 */
  baseUrl: string
  maskedKey: string
  state: ImportRowState
  reason?: string
  warnings: string[]
  request?: CreateAccountRequest
}

export interface ExistingAccountRef {
  id: number
  name: string
  baseUrl: string
  maskedApiKey?: string
}

/** 合并默认参数：文件里有的字段优先，缺的用默认值；标签和支持模型文件里为空时才用默认。 */
export function applyImportDefaults(
  account: Partial<CreateAccountRequest> & { baseUrl: string; apiKey: string },
  defaults: ImportDefaults,
): CreateAccountRequest {
  const request: CreateAccountRequest = {
    ...account,
    name: account.name?.trim() || defaultImportName(account.baseUrl, account.apiKey),
    baseUrl: account.baseUrl.trim(),
    apiKey: account.apiKey.trim(),
    enabled: defaults.enabled,
    authType: account.authType ?? defaults.authType,
    usageProjectionMode: account.usageProjectionMode ?? defaults.usageProjectionMode,
  }
  const priority = optionalNumber(defaults.priority)
  if (request.priority === undefined && priority !== undefined) request.priority = priority
  const concurrency = optionalNumber(defaults.maxConcurrentRequests)
  if (request.maxConcurrentRequests === undefined && concurrency !== undefined) request.maxConcurrentRequests = concurrency
  const multiplier = optionalNumber(defaults.costMultiplier)
  if (request.costMultiplier === undefined && multiplier !== undefined) request.costMultiplier = multiplier
  if (!request.tags?.length && defaults.tags.length) request.tags = normalizeTags(defaults.tags)
  if (!request.supportedModels?.length && defaults.supportedModels.length) request.supportedModels = [...defaults.supportedModels]
  return request
}

/**
 * 生成导入计划：格式错误的标为 invalid；同一批里 baseUrl + apiKey 重复的只保留第一条；
 * 账号池里已有的按“地址相同、密钥脱敏值相同”判定（列表接口只返回脱敏密钥），短密钥脱敏后都是 ***，不参与比较。
 */
export function buildImportPlan(
  inputs: ReadonlyArray<{ origin: string; entries: readonly ParsedImportEntry[] }>,
  defaults: ImportDefaults,
  existing: readonly ExistingAccountRef[],
): ImportPlanRow[] {
  const existingByKey = new Map<string, ExistingAccountRef>()
  for (const account of existing) {
    if (!account.maskedApiKey || account.maskedApiKey === '***') continue
    const key = `${normalizeBaseUrl(account.baseUrl)}\n${account.maskedApiKey}`
    if (!existingByKey.has(key)) existingByKey.set(key, account)
  }
  const firstSeen = new Map<string, number>()
  const rows: ImportPlanRow[] = []
  for (const input of inputs) {
    for (const entry of input.entries) {
      if (!entry.account) {
        rows.push({ origin: input.origin, position: entry.position, name: '-', baseUrl: '-', maskedKey: '-', state: 'invalid', reason: entry.error ?? '格式错误', warnings: entry.warnings })
        continue
      }
      const request = applyImportDefaults(entry.account, defaults)
      const maskedKey = maskApiKey(request.apiKey)
      const row: ImportPlanRow = {
        origin: input.origin,
        position: entry.position,
        name: request.name,
        baseUrl: displayBaseUrl(request.baseUrl),
        maskedKey,
        state: 'new',
        warnings: entry.warnings,
        request,
      }
      const key = importDedupKey(request.baseUrl, request.apiKey)
      const duplicateOf = firstSeen.get(key)
      if (duplicateOf !== undefined) {
        const first = rows[duplicateOf]
        row.state = 'duplicate'
        row.reason = `与 ${first.origin} 第 ${first.position} 条重复`
      } else {
        firstSeen.set(key, rows.length)
        const found = maskedKey !== '***' ? existingByKey.get(`${normalizeBaseUrl(request.baseUrl)}\n${maskedKey}`) : undefined
        if (found) {
          row.state = 'exists'
          row.reason = `账号池里已有 #${found.id}「${found.name}」（地址相同、密钥脱敏值一致）`
        }
      }
      rows.push(row)
    }
  }
  return rows
}

export function countImportRows(rows: readonly Pick<ImportPlanRow, 'state'>[]): Record<ImportRowState, number> {
  const counts: Record<ImportRowState, number> = { new: 0, duplicate: 0, exists: 0, invalid: 0 }
  for (const row of rows) counts[row.state] += 1
  return counts
}
