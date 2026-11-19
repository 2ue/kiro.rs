// 账号风控页从 URL 读初始查询条件：账号卡片的“诊断”跳过来时带 accountId，也可以带时间窗口。
// 认的参数：accountId（或 account）、window（today / last24h / yesterday / last7d / custom），
// since / until（或 from / to，ISO 时间）。
// - 没给 window（或 window 不认识）但给了 since / until：按自定义窗口处理，不丢掉显式给的时间。
// - 给了 today 等固定窗口：以窗口为准，忽略 since / until。
// - 自定义窗口只给一端时补齐 24 小时：只给 until → since = until - 24h；
//   只给 since → until = min(since + 24h, 现在)，since 在未来时不截到现在（否则会早于 since），直接取 since + 24h。
// - since 晚于 until：两端交换。两端都是用户显式写的，多半只是顺序写反，交换后两端都保留；
//   忽略任一端都会丢掉用户的输入。

export const RISK_WINDOW_KEYS = ['today', 'last24h', 'yesterday', 'last7d', 'custom'] as const
export type RiskWindowKey = (typeof RISK_WINDOW_KEYS)[number]

export interface RiskUrlInitial {
  windowKey: RiskWindowKey
  accountId?: number
  since?: Date
  until?: Date
}

const DEFAULT_WINDOW: RiskWindowKey = 'last24h'
const DAY_MS = 24 * 60 * 60 * 1000

function positiveInt(value: string | null): number | undefined {
  if (!value) return undefined
  const trimmed = value.trim().replace(/^#/, '')
  if (!/^\d+$/.test(trimmed)) return undefined
  const parsed = Number(trimmed)
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : undefined
}

function dateParam(value: string | null): Date | undefined {
  if (!value?.trim()) return undefined
  const date = new Date(value.trim())
  return Number.isNaN(date.getTime()) ? undefined : date
}

function isWindowKey(value: string | undefined): value is RiskWindowKey {
  return (RISK_WINDOW_KEYS as readonly string[]).includes(value ?? '')
}

export function riskInitialFromSearch(params: URLSearchParams, now: Date = new Date()): RiskUrlInitial {
  const accountId = positiveInt(params.get('accountId') ?? params.get('account'))
  const rawWindow = (params.get('window') ?? params.get('windowKey'))?.trim()
  // 不认识的 window 当作没给：下面按 since / until 是否出现决定是否走自定义窗口。
  const explicitWindow = isWindowKey(rawWindow) ? rawWindow : undefined
  let since = dateParam(params.get('since') ?? params.get('from'))
  let until = dateParam(params.get('until') ?? params.get('to'))
  const windowKey: RiskWindowKey = explicitWindow ?? (since || until ? 'custom' : DEFAULT_WINDOW)
  if (windowKey !== 'custom') return { windowKey, accountId, since: undefined, until: undefined }

  if (since && !until) {
    const end = Math.min(since.getTime() + DAY_MS, now.getTime())
    until = new Date(end >= since.getTime() ? end : since.getTime() + DAY_MS)
  } else if (until && !since) {
    since = new Date(until.getTime() - DAY_MS)
  }
  if (since && until && since.getTime() > until.getTime()) [since, until] = [until, since]
  return { windowKey, accountId, since, until }
}

/** 账号卡片“诊断”跳转的地址。 */
export function accountRiskLink(accountId: number): string {
  return `/account-risk?${new URLSearchParams({ accountId: String(accountId) }).toString()}`
}
