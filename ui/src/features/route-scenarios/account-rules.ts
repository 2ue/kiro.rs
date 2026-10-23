// Pure helpers behind the scenario account editor. Kept free of runtime imports so they can be
// unit tested with `node --test` (see ui/tests/).
import type { AccountRoutePolicy, RoutePolicy, RouteScenarioAccountOption } from '@/types/api'

export function policySummary(policy: RoutePolicy): string {
  if (policy.routeMode === 'allow_all') return '不限制'
  const routes = policy.routeRules.join('、') || '（未选路由）'
  return policy.routeMode === 'allow_list' ? `仅允许 ${routes}` : `禁止 ${routes}`
}

/** Identity of a rule regardless of route order or case, so equal rules land in one group. */
export function policyKey(policy: RoutePolicy): string {
  if (policy.routeMode === 'allow_all') return 'allow_all'
  const routes = Array.from(new Set(policy.routeRules.map((route) => route.trim().toLowerCase()))).sort()
  return `${policy.routeMode}:${routes.join('\n')}`
}

export type RuleGroup = { key: string; summary: string; count: number }

/** Accounts grouped by rule, largest group first. */
export function accountRuleGroups(value: AccountRoutePolicy[]): RuleGroup[] {
  const groups = new Map<string, RuleGroup>()
  for (const entry of value) {
    const key = policyKey(entry)
    const group = groups.get(key)
    if (group) group.count += 1
    else groups.set(key, { key, summary: policySummary(entry), count: 1 })
  }
  return Array.from(groups.values()).sort((a, b) => b.count - a.count || a.summary.localeCompare(b.summary))
}

/** Writes one rule to every given account, replacing what they had; "unrestricted" removes them. */
export function assignPolicy(value: AccountRoutePolicy[], ids: Iterable<number>, policy: RoutePolicy): AccountRoutePolicy[] {
  const targets = new Set(ids)
  const kept = value.filter((entry) => !targets.has(entry.id))
  const assigned = policy.routeMode === 'allow_all'
    ? []
    : Array.from(targets, (id) => ({ id, routeMode: policy.routeMode, routeRules: [...policy.routeRules] }))
  return [...kept, ...assigned].sort((a, b) => a.id - b.id)
}

export type AccountRow = { id: number; label: string; missing: boolean; rule: AccountRoutePolicy | undefined }

/** Every account of the pool with its scenario rule; rules for deleted accounts come first. */
export function buildAccountRows(accounts: RouteScenarioAccountOption[], value: AccountRoutePolicy[]): AccountRow[] {
  const rules = new Map(value.map((entry) => [entry.id, entry]))
  const known = new Set(accounts.map((account) => account.id))
  return [
    ...value
      .filter((entry) => !known.has(entry.id))
      .map((entry) => ({ id: entry.id, label: '账号已不存在', missing: true, rule: entry })),
    ...accounts.map((account) => ({ id: account.id, label: account.label, missing: false, rule: rules.get(account.id) })),
  ]
}

/** `all`, `unset`, `set`, or `rule:<policyKey>`. */
export type RowFilter = string

export function filterRows(rows: AccountRow[], filter: RowFilter, search: string): AccountRow[] {
  const keyword = search.trim().toLowerCase()
  return rows.filter((row) => {
    if (filter === 'unset' && row.rule) return false
    if (filter === 'set' && !row.rule) return false
    if (filter.startsWith('rule:') && (!row.rule || `rule:${policyKey(row.rule)}` !== filter)) return false
    if (!keyword) return true
    const summary = row.rule ? policySummary(row.rule).toLowerCase() : '未设置'
    return row.label.toLowerCase().includes(keyword) || String(row.id).includes(keyword) || summary.includes(keyword)
  })
}

/**
 * Next selection after clicking row `index` of `visible`. With shift and a previous click still
 * in the list, the whole range takes the clicked row's new state.
 */
export function selectByClick(
  selected: ReadonlySet<number>,
  visible: AccountRow[],
  index: number,
  anchorId: number | null,
  shift: boolean,
): Set<number> {
  const row = visible[index]
  const next = new Set(selected)
  if (!row) return next
  const checked = !selected.has(row.id)
  const anchor = anchorId === null ? -1 : visible.findIndex((item) => item.id === anchorId)
  const range = shift && anchor >= 0 ? visible.slice(Math.min(anchor, index), Math.max(anchor, index) + 1) : [row]
  for (const item of range) {
    if (checked) next.add(item.id)
    else next.delete(item.id)
  }
  return next
}

/** Rule the batch dialog starts from: the targets' shared rule, otherwise an empty deny list. */
export function initialBatchPolicy(targets: AccountRow[]): RoutePolicy {
  const keys = new Set(targets.map((row) => (row.rule ? policyKey(row.rule) : '')))
  const shared = keys.size === 1 ? targets[0]?.rule : undefined
  return shared ? { routeMode: shared.routeMode, routeRules: [...shared.routeRules] } : { routeMode: 'deny_list', routeRules: [] }
}
