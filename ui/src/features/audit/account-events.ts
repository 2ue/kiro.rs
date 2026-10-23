// Account change view helpers. No runtime imports so they can be tested with `node --test`.
import type { AccountEventSummaryRow, AdminAuditLogPageQuery, AdminAuditLogRow } from '@/types/api'

export type EventTone = 'error' | 'warning' | 'success' | 'info' | 'neutral'

export const ACCOUNT_EVENT_META: Record<string, { label: string; tone: EventTone }> = {
  quota_exhausted: { label: '额度耗尽', tone: 'error' },
  overage_started: { label: '开始超额', tone: 'warning' },
  auto_disabled: { label: '自动禁用', tone: 'error' },
  risk_controlled: { label: '风控/封禁', tone: 'error' },
  quota_recovered: { label: '额度恢复', tone: 'success' },
  auto_enabled: { label: '自动恢复', tone: 'success' },
  manual_disabled: { label: '手动禁用', tone: 'warning' },
  manual_enabled: { label: '手动启用', tone: 'info' },
  deleted: { label: '删除账号', tone: 'neutral' },
}

/** Display order of the kind filter and the summary tiles: problems first. */
export const ACCOUNT_EVENT_ORDER = [
  'quota_exhausted',
  'auto_disabled',
  'risk_controlled',
  'overage_started',
  'quota_recovered',
  'auto_enabled',
  'manual_disabled',
  'manual_enabled',
  'deleted',
] as const

export const TIME_RANGES = [
  { value: '1', label: '最近 1 小时' },
  { value: '24', label: '最近 24 小时' },
  { value: '168', label: '最近 7 天' },
  { value: '720', label: '最近 30 天' },
  { value: 'all', label: '全部时间' },
] as const

export const ACTOR_LABELS: Record<string, string> = {
  'admin-api': '管理员',
  'system-scheduler': '调度器',
  'system-account-info': '额度查询',
  'system-external-pool': '外部池',
}

export function accountEventMeta(kind: string | undefined): { label: string; tone: EventTone } {
  return (kind && ACCOUNT_EVENT_META[kind]) || { label: kind || '未知', tone: 'neutral' }
}

export interface AccountEventView {
  kind: string
  label: string
  tone: EventTone
  source: 'local' | 'external' | string
  accountId: string
  accountLabel: string
  reason: string
  actor: string
}

function text(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

/** Reads the account-change fields out of an audit row; `null` when the row is not one. */
export function accountEventView(record: AdminAuditLogRow): AccountEventView | null {
  const detail = (record.detail && typeof record.detail === 'object' ? record.detail : {}) as Record<string, unknown>
  const kind = text(detail.accountEvent)
  if (!kind) return null
  const meta = accountEventMeta(kind)
  return {
    kind,
    label: meta.label,
    tone: meta.tone,
    source: text(detail.accountSource) || (record.objectType === 'external_pool' ? 'external' : 'local'),
    accountId: record.objectId ?? '',
    accountLabel: text(detail.accountLabel) || text(detail.credentialLabel) || (record.objectId ? `#${record.objectId}` : '—'),
    reason: text(detail.reasonLabel) || text(detail.message) || text(detail.reason),
    actor: ACTOR_LABELS[record.actor] ?? record.actor,
  }
}

export interface AccountFilterState {
  range: string
  kind: string
  source: string
  account: string
}

/** Query for the account view. `now` is injected so the window is testable. */
export function accountEventQuery(state: AccountFilterState, page: number, limit: number, now: Date = new Date()): AdminAuditLogPageQuery {
  const query: AdminAuditLogPageQuery = { page, limit, accountEvents: true }
  if (state.range !== 'all') {
    const hours = Number(state.range)
    if (Number.isFinite(hours) && hours > 0) query.since = new Date(now.getTime() - hours * 3_600_000).toISOString()
  }
  if (state.kind !== '__all__') query.accountEvent = state.kind
  if (state.source !== '__all__') query.accountSource = state.source
  const account = state.account.trim()
  if (account) query.account = account
  return query
}

/** Summary tiles in display order; kinds without events are left out. */
export function summaryTiles(items: AccountEventSummaryRow[]): { kind: string; label: string; tone: EventTone; events: number; accounts: number }[] {
  const byKind = new Map(items.map((item) => [item.event, item]))
  return ACCOUNT_EVENT_ORDER.flatMap((kind) => {
    const item = byKind.get(kind)
    if (!item || item.events <= 0) return []
    const meta = accountEventMeta(kind)
    return [{ kind, label: meta.label, tone: meta.tone, events: item.events, accounts: item.accounts }]
  })
}
