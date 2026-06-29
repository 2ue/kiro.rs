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

function normalizeKamAccount(item: unknown): unknown {
  const normalized = camelizeKeys(item)
  if (typeof normalized !== 'object' || normalized === null) return normalized
  const obj = normalized as Record<string, unknown>
  if (typeof obj.refreshToken === 'string' && typeof obj.credentials === 'undefined') {
    return {
      email: typeof obj.email === 'string' ? obj.email : undefined,
      userId: typeof obj.userId === 'string' || obj.userId === null ? (obj.userId as string | null) : undefined,
      nickname: typeof obj.nickname === 'string' ? obj.nickname : typeof obj.label === 'string' ? obj.label : undefined,
      status: typeof obj.status === 'string' ? obj.status : undefined,
      machineId: typeof obj.machineId === 'string' ? obj.machineId : undefined,
      credentials: {
        accessToken: typeof obj.accessToken === 'string' ? obj.accessToken : undefined,
        expiresAt: typeof obj.expiresAt === 'string' ? obj.expiresAt : typeof obj.expired === 'string' ? obj.expired : undefined,
        refreshToken: obj.refreshToken,
        clientId: typeof obj.clientId === 'string' ? obj.clientId : undefined,
        clientSecret: typeof obj.clientSecret === 'string' ? obj.clientSecret : undefined,
        tokenEndpoint: typeof obj.tokenEndpoint === 'string' ? obj.tokenEndpoint : undefined,
        issuerUrl: typeof obj.issuerUrl === 'string' ? obj.issuerUrl : undefined,
        scopes: typeof obj.scopes === 'string' ? obj.scopes : typeof obj.scope === 'string' ? obj.scope : undefined,
        profileArn: typeof obj.profileArn === 'string' ? obj.profileArn : undefined,
        region: typeof obj.region === 'string' ? obj.region : undefined,
        apiRegion: typeof obj.apiRegion === 'string' ? obj.apiRegion : undefined,
        authMethod: typeof obj.authMethod === 'string' ? obj.authMethod : undefined,
        startUrl: typeof obj.startUrl === 'string' ? obj.startUrl : undefined,
      },
    }
  }
  return item
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
