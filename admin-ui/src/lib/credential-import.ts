import type { AddCredentialRequest } from '@/types/api'

type JsonObject = Record<string, unknown>

export interface CredentialFileImportResult {
  credentials: AddCredentialRequest[]
  errors: string[]
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function stringField(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined
}

function numberField(value: unknown): number | undefined {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return Math.max(0, Math.trunc(value))
  }
  if (typeof value === 'string' && value.trim()) {
    const parsed = Number(value)
    if (Number.isFinite(parsed)) {
      return Math.max(0, Math.trunc(parsed))
    }
  }
  return undefined
}

function authMethodField(value: unknown): AddCredentialRequest['authMethod'] | undefined {
  const method = stringField(value)
  if (method === 'social' || method === 'idc' || method === 'api_key') {
    return method
  }
  return undefined
}

function parseJsonOrJsonl(text: string): unknown[] {
  const trimmed = text.trim()
  if (!trimmed) {
    return []
  }

  try {
    return [JSON.parse(trimmed)]
  } catch (jsonError) {
    const lines = trimmed
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean)

    if (lines.length <= 1) {
      throw jsonError
    }

    return lines.map((line, index) => {
      try {
        return JSON.parse(line)
      } catch (lineError) {
        throw new Error(`第 ${index + 1} 行 JSONL 格式错误: ${lineError instanceof Error ? lineError.message : String(lineError)}`)
      }
    })
  }
}

function extractCredentialItems(value: unknown): unknown[] {
  if (Array.isArray(value)) {
    return value.flatMap(extractCredentialItems)
  }
  if (!isObject(value)) {
    return []
  }

  if (Array.isArray(value.credentials)) {
    return value.credentials.flatMap(extractCredentialItems)
  }
  if (Array.isArray(value.accounts)) {
    return value.accounts.flatMap(extractCredentialItems)
  }
  if (isObject(value.data) && Array.isArray(value.data.credentials)) {
    return value.data.credentials.flatMap(extractCredentialItems)
  }

  return [value]
}

export function normalizeCredentialImportItem(value: unknown): AddCredentialRequest | null {
  if (!isObject(value)) {
    return null
  }

  const nested = isObject(value.credentials) ? value.credentials : undefined
  const refreshToken = stringField(value.refreshToken) ?? stringField(nested?.refreshToken)
  const kiroApiKey =
    stringField(value.kiroApiKey) ??
    stringField(value.apiKey) ??
    stringField(value.kiro_api_key)
  const clientId = stringField(value.clientId) ?? stringField(nested?.clientId)
  const clientSecret = stringField(value.clientSecret) ?? stringField(nested?.clientSecret)
  const authRegion =
    stringField(value.authRegion) ??
    stringField(nested?.authRegion) ??
    stringField(value.region) ??
    stringField(nested?.region)
  const apiRegion = stringField(value.apiRegion) ?? stringField(nested?.apiRegion)
  const rawAuthMethod = authMethodField(value.authMethod) ?? authMethodField(nested?.authMethod)
  const authMethod: AddCredentialRequest['authMethod'] = kiroApiKey
    ? 'api_key'
    : rawAuthMethod ?? (clientId && clientSecret ? 'idc' : 'social')

  if (authMethod === 'api_key') {
    if (!kiroApiKey) {
      return null
    }
  } else if (!refreshToken) {
    return null
  }

  return {
    authMethod,
    refreshToken: authMethod === 'api_key' ? undefined : refreshToken,
    kiroApiKey: authMethod === 'api_key' ? kiroApiKey : undefined,
    clientId: authMethod === 'api_key' ? undefined : clientId,
    clientSecret: authMethod === 'api_key' ? undefined : clientSecret,
    email: stringField(value.email) ?? stringField(value.nickname),
    priority: numberField(value.priority),
    authRegion,
    apiRegion,
    machineId: stringField(value.machineId) ?? stringField(nested?.machineId),
    proxyUrl: stringField(value.proxyUrl) ?? stringField(nested?.proxyUrl),
    proxyUsername: stringField(value.proxyUsername) ?? stringField(nested?.proxyUsername),
    proxyPassword: stringField(value.proxyPassword) ?? stringField(nested?.proxyPassword),
    proxyResourceId: numberField(value.proxyResourceId) ?? numberField(nested?.proxyResourceId),
    endpoint: stringField(value.endpoint) ?? stringField(nested?.endpoint),
  }
}

export function parseCredentialImportText(text: string): AddCredentialRequest[] {
  return parseJsonOrJsonl(text)
    .flatMap(extractCredentialItems)
    .map(normalizeCredentialImportItem)
    .filter((credential): credential is AddCredentialRequest => Boolean(credential))
}

export async function parseCredentialImportFiles(files: File[]): Promise<CredentialFileImportResult> {
  const credentials: AddCredentialRequest[] = []
  const errors: string[] = []

  for (const file of files) {
    try {
      const text = await file.text()
      const parsed = parseCredentialImportText(text)
      if (parsed.length === 0) {
        errors.push(`${file.name}: 未找到有效凭据`)
      } else {
        credentials.push(...parsed)
      }
    } catch (error) {
      errors.push(`${file.name}: ${error instanceof Error ? error.message : String(error)}`)
    }
  }

  return { credentials, errors }
}
