import type { AddCredentialRequest } from '@/api/types'

/**
 * 导入默认参数：字段与现有 ui/ 的批量导入 / KAM 导入保持一致。
 * 只填充每条账号中缺失的字段；账号自身的值优先。
 */
export type ProxyAssignmentMode = 'single' | 'round_robin' | 'none'
export type VerifyMode = 'subscription_only' | 'model_and_subscription'

export interface ImportDefaults {
  disabled: boolean
  enableOverageAfterImport: boolean
  tags: string[]
  priority: string
  maxConcurrentRequests: string
  rpm: string
  region: string
  authRegion: string
  apiRegion: string
  machineId: string
  endpoint: string
  proxyMode: ProxyAssignmentMode
  proxyResourceId: string
  proxyResourceIds: string[]
  proxyUrl: string
  proxyUsername: string
  proxyPassword: string
}

export interface ImportOptions {
  autoDiscoverSupportedModels: boolean
  /** 跳过来源文件中 status 为 error 的账号（KAM 导出） */
  skipErrorAccounts: boolean
  skipVerify: boolean
  verifyMode: VerifyMode
  /** 测试模型后同步查询订阅/积分 */
  refreshInfoAfterModelTest: boolean
}

export function initialDefaults(): ImportDefaults {
  return {
    disabled: false,
    enableOverageAfterImport: false,
    tags: [],
    priority: '',
    maxConcurrentRequests: '',
    rpm: '',
    region: '',
    authRegion: '',
    apiRegion: '',
    machineId: '',
    endpoint: '',
    proxyMode: 'single',
    proxyResourceId: '',
    proxyResourceIds: [],
    proxyUrl: '',
    proxyUsername: '',
    proxyPassword: '',
  }
}

export function initialOptions(): ImportOptions {
  return {
    autoDiscoverSupportedModels: false,
    skipErrorAccounts: true,
    skipVerify: false,
    verifyMode: 'subscription_only',
    refreshInfoAfterModelTest: false,
  }
}

const trimmed = (v: unknown): string | undefined => {
  const t = typeof v === 'string' ? v.trim() : typeof v === 'number' && Number.isFinite(v) ? String(Math.trunc(v)) : ''
  return t || undefined
}

function nonNegativeInt(value: string, label: string): number | undefined {
  const t = value.trim()
  if (!t) return undefined
  const n = Number(t)
  if (!Number.isInteger(n) || n < 0) throw new Error(`${label}必须是非负整数`)
  return n
}

/** 校验默认参数，返回错误文本；无误返回 undefined */
export function validateDefaults(d: ImportDefaults): string | undefined {
  if (d.proxyMode === 'round_robin' && d.proxyResourceIds.length === 0) return '已选择多个代理轮换，请至少选择一个代理资源'
  try {
    nonNegativeInt(d.priority, '默认优先级')
    nonNegativeInt(d.maxConcurrentRequests, '默认账号并发')
    nonNegativeInt(d.rpm, '默认账号 RPM')
  } catch (e) {
    return (e as Error).message
  }
  return undefined
}

/**
 * 合并默认参数到单个账号。importIndex 是该账号在整份名单中的位置，
 * 用于多代理轮换时按顺序循环分配。
 */
export function mergeDefaults(cred: AddCredentialRequest, d: ImportDefaults, importIndex: number): AddCredentialRequest {
  const roundRobinId =
    d.proxyMode === 'round_robin' && d.proxyResourceIds.length
      ? nonNegativeInt(d.proxyResourceIds[importIndex % d.proxyResourceIds.length]!, '代理资源 ID')
      : undefined
  const defaultProxyId = roundRobinId ?? (d.proxyMode === 'single' ? nonNegativeInt(d.proxyResourceId, '代理资源 ID') : undefined)
  const hasDirectProxy = Boolean(trimmed(cred.proxyUrl) || trimmed(cred.proxyUsername) || trimmed(cred.proxyPassword))
  const proxyResourceId = cred.proxyResourceId !== undefined ? cred.proxyResourceId : hasDirectProxy ? undefined : defaultProxyId
  const useResource = typeof proxyResourceId === 'number'
  const directDefaults = d.proxyMode === 'single' && !useResource
  return {
    ...cred,
    disabled: cred.disabled === undefined || cred.disabled === null ? d.disabled : cred.disabled,
    priority: cred.priority ?? nonNegativeInt(d.priority, '默认优先级'),
    maxConcurrentRequests:
      cred.maxConcurrentRequests === undefined ? nonNegativeInt(d.maxConcurrentRequests, '默认账号并发') : cred.maxConcurrentRequests,
    rpm: cred.rpm === undefined ? nonNegativeInt(d.rpm, '默认账号 RPM') : cred.rpm,
    region: trimmed(cred.region) || trimmed(d.region),
    authRegion: trimmed(cred.authRegion) || trimmed(d.authRegion),
    apiRegion: trimmed(cred.apiRegion) || trimmed(d.apiRegion),
    machineId: trimmed(cred.machineId) || trimmed(d.machineId),
    endpoint: trimmed(cred.endpoint) || trimmed(d.endpoint),
    enableOverageAfterImport:
      cred.enableOverageAfterImport === undefined || cred.enableOverageAfterImport === null
        ? d.enableOverageAfterImport
        : cred.enableOverageAfterImport,
    proxyResourceId,
    proxyUrl: trimmed(cred.proxyUrl) || (directDefaults ? trimmed(d.proxyUrl) : undefined),
    proxyUsername: trimmed(cred.proxyUsername) || (directDefaults ? trimmed(d.proxyUsername) : undefined),
    proxyPassword: trimmed(cred.proxyPassword) || (directDefaults ? trimmed(d.proxyPassword) : undefined),
    tags: cred.tags?.length ? cred.tags : d.tags,
  }
}
