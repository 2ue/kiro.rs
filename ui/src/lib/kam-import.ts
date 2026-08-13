import { extractErrorMessage } from '@/lib/utils'
import { camelizeKeys } from '@/lib/object-keys'

export interface KamAccount {
  email?: string
  userId?: string | null
  nickname?: string
  credentials: {
    accessToken?: string
    expiresAt?: string
    refreshToken: string
    clientId?: string
    clientSecret?: string
    tokenEndpoint?: string
    issuerUrl?: string
    scopes?: string
    profileArn?: string
    region?: string
    apiRegion?: string
    authMethod?: string
    startUrl?: string
  }
  machineId?: string
  status?: string
}

type JsonObject = Record<string, unknown>

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function stringField(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined
}

function profileArnRegion(profileArn: string | undefined): string | undefined {
  if (!profileArn) return undefined
  const parts = profileArn.trim().split(':')
  if (parts.length < 6 || parts[0] !== 'arn' || parts[2] !== 'codewhisperer') return undefined
  return parts[3]?.trim() || undefined
}

function stringLikeField(value: unknown): string | undefined {
  if (typeof value === 'string' && value.trim()) {
    const trimmed = value.trim()
    if (/^\d+(\.\d+)?$/.test(trimmed)) return timestampToIsoString(Number(trimmed))
    return trimmed
  }
  if (typeof value === 'number' && Number.isFinite(value)) return timestampToIsoString(value)
  return undefined
}

function timestampToIsoString(value: number): string | undefined {
  if (!Number.isFinite(value)) return undefined
  const millis = value > 10_000_000_000 ? value : value * 1000
  const date = new Date(millis)
  return Number.isFinite(date.getTime()) ? date.toISOString() : undefined
}

function normalizeKamAccount(item: unknown): unknown {
  const normalized = camelizeKeys(item)
  if (!isObject(normalized)) return normalized
  const obj = normalized
  const nested = isObject(obj.credentials) ? obj.credentials : undefined
  const source = nested ?? obj
  const refreshToken = stringField(source.refreshToken)
  if (!refreshToken) return normalized
  const profileArn = stringField(source.profileArn)

  return {
    email: stringField(obj.email),
    userId: typeof obj.userId === 'string' || obj.userId === null ? (obj.userId as string | null) : undefined,
    nickname: stringField(obj.nickname) ?? stringField(obj.label),
    status: stringField(obj.status),
    machineId: stringField(obj.machineId) ?? stringField(source.machineId),
    credentials: {
      accessToken: stringField(source.accessToken),
      expiresAt: stringLikeField(source.expiresAt) ?? stringLikeField(source.expired),
      refreshToken,
      clientId: stringField(source.clientId),
      clientSecret: stringField(source.clientSecret),
      tokenEndpoint: stringField(source.tokenEndpoint),
      issuerUrl: stringField(source.issuerUrl),
      scopes: stringField(source.scopes) ?? stringField(source.scope),
      profileArn,
      region: stringField(source.region),
      apiRegion: stringField(source.apiRegion) ?? profileArnRegion(profileArn),
      authMethod: stringField(source.authMethod),
      startUrl: stringField(source.startUrl),
    },
  }
}

function isValidKamAccount(item: unknown): item is KamAccount {
  if (typeof item !== 'object' || item === null) return false
  const obj = item as Record<string, unknown>
  if (typeof obj.credentials !== 'object' || obj.credentials === null) return false
  const cred = obj.credentials as Record<string, unknown>
  return typeof cred.refreshToken === 'string' && cred.refreshToken.trim().length > 0
}

export function parseKamJson(raw: string): KamAccount[] {
  const parsed = camelizeKeys(JSON.parse(raw)) as Record<string, unknown>
  let rawItems: unknown[]
  if (parsed.accounts && Array.isArray(parsed.accounts)) rawItems = parsed.accounts
  else if (Array.isArray(parsed)) rawItems = parsed
  else if (parsed.credentials && typeof parsed.credentials === 'object') rawItems = [parsed]
  else if (typeof parsed.refreshToken === 'string') rawItems = [parsed]
  else throw new Error('无法识别的 KAM JSON 格式')

  const validAccounts = rawItems.map(normalizeKamAccount).filter(isValidKamAccount)
  if (rawItems.length > 0 && validAccounts.length === 0) {
    throw new Error(`共 ${rawItems.length} 条记录，但均缺少有效的 credentials.refreshToken`)
  }
  return validAccounts
}

export async function parseKamFiles(files: File[]): Promise<{ accounts: KamAccount[]; errors: string[] }> {
  const accounts: KamAccount[] = []
  const errors: string[] = []
  for (const file of files) {
    try {
      const parsed = parseKamJson(await file.text())
      if (parsed.length === 0) errors.push(`${file.name}: 未找到有效账号`)
      else accounts.push(...parsed)
    } catch (error) {
      errors.push(`${file.name}: ${extractErrorMessage(error)}`)
    }
  }
  return { accounts, errors }
}
