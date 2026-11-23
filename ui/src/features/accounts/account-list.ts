// 账号列表的筛选、排序、分类计数和选择：账号列表与 /accounts/status 都在前端，纯函数便于单测。
import type { Account, AccountStatus } from '@/types/api'

export type EnabledFilter = 'all' | 'enabled' | 'disabled'
export type DispatchFilter = 'all' | 'dispatchable' | 'blocked'
export type RuntimeFlag = 'cooldown' | 'concurrency_full' | 'probation' | 'failing' | 'auto_disabled'
export type ProxyFilter = 'all' | 'with_proxy' | 'without_proxy'
export type AccountSortBy =
  | 'default'
  | 'priority'
  | 'name'
  | 'id'
  | 'created_at'
  | 'updated_at'
  | 'dispatched'
  | 'in_flight'
  | 'error_rate'
  | 'ttft'
  | 'cost_multiplier'
export type SortOrder = 'asc' | 'desc'

export const MODEL_FILTER_ALL = '__all__'
export const MODEL_FILTER_UNRESTRICTED = '__unrestricted__'
export const TAG_FILTER_ALL = '__all__'

export const RUNTIME_FLAG_OPTIONS: Array<{ value: RuntimeFlag; label: string; title: string }> = [
  { value: 'cooldown', label: '冷却中', title: '出错后暂停调度，冷却结束自动恢复' },
  { value: 'concurrency_full', label: '并发已满', title: '在飞请求数已达到单账号最大并发' },
  { value: 'probation', label: '观察期', title: '质量感知调度判定退化，正在避让' },
  { value: 'failing', label: '近期失败', title: '失败窗口内有连续的瞬态失败' },
  { value: 'auto_disabled', label: '自动禁用', title: '命中自动禁用规则，需要清除或等待到期' },
]

export const SORT_OPTIONS: Array<{ value: AccountSortBy; label: string }> = [
  { value: 'default', label: '默认排序' },
  { value: 'priority', label: '优先级' },
  { value: 'name', label: '名称' },
  { value: 'id', label: 'ID' },
  { value: 'created_at', label: '创建时间' },
  { value: 'updated_at', label: '更新时间' },
  { value: 'dispatched', label: '调度次数（本实例）' },
  { value: 'in_flight', label: '并发占用' },
  { value: 'error_rate', label: '质量错误率' },
  { value: 'ttft', label: '首字延迟' },
  { value: 'cost_multiplier', label: '成本倍率' },
]

export interface AccountListFilters {
  keyword: string
  tag: string
  enabled: EnabledFilter
  dispatch: DispatchFilter
  runtime: RuntimeFlag[]
  model: string
  proxy: ProxyFilter
}

export const DEFAULT_ACCOUNT_FILTERS: AccountListFilters = {
  keyword: '',
  tag: TAG_FILTER_ALL,
  enabled: 'all',
  dispatch: 'all',
  runtime: [],
  model: MODEL_FILTER_ALL,
  proxy: 'all',
}

export type StatusMap = ReadonlyMap<number, AccountStatus>

/** 与后端 is_auto_disabled_now 一致：自动禁用且没有到期时间或还没到期。 */
export function isAutoDisabledNow(account: Pick<Account, 'autoDisabled' | 'autoDisabledUntil'>, now = Date.now()): boolean {
  if (!account.autoDisabled) return false
  if (!account.autoDisabledUntil) return true
  const until = Date.parse(account.autoDisabledUntil)
  return Number.isNaN(until) || until > now
}

/** 账号当前命中的运行状态；运行数据来自 /accounts/status，缺失时只按账号自身字段判断。 */
export function accountRuntimeFlags(account: Account, status: AccountStatus | undefined, now = Date.now()): Set<RuntimeFlag> {
  const flags = new Set<RuntimeFlag>()
  if ((status?.cooldownRemainingSecs ?? 0) > 0) flags.add('cooldown')
  // 与后端 skip_reason 一致：并发上限按至少 1 算。
  if (status && status.inFlight >= Math.max(account.maxConcurrentRequests, 1)) flags.add('concurrency_full')
  if (status?.quality?.inProbation) flags.add('probation')
  if ((status?.transientFailureStreak ?? 0) > 0) flags.add('failing')
  if (isAutoDisabledNow(account, now)) flags.add('auto_disabled')
  return flags
}

export function isDispatchable(status: AccountStatus | undefined): boolean {
  return status?.dispatchable === true
}

export interface AccountFacetCounts {
  enabled: Record<EnabledFilter, number>
  dispatch: Record<DispatchFilter, number>
  runtime: Record<RuntimeFlag, number>
}

/**
 * 分段计数随其它筛选变化，口径对齐 main 的 credential_filter_facets：
 * - 启用状态：套用除“启用状态”和“调度状况”以外的全部筛选后计数（main 的 enable_query 同时去掉 enabled 和 status）；
 * - 调度状况：套用除“调度状况”以外的全部筛选后计数（main 的 health_query 只去掉 status）；
 * - 运行状态：每一项都套用除“运行状态”以外的全部筛选后计数，选了别的运行状态也不影响各项数字。
 * 不传 filters（或传默认筛选）就是全局计数，顶部统计卡用这个。
 */
export function accountFacetCounts(
  accounts: readonly Account[],
  statuses: StatusMap,
  filters: AccountListFilters = DEFAULT_ACCOUNT_FILTERS,
  now = Date.now(),
): AccountFacetCounts {
  const enabledFilters: AccountListFilters = { ...filters, enabled: 'all', dispatch: 'all' }
  const dispatchFilters: AccountListFilters = { ...filters, dispatch: 'all' }
  const runtimeFilters: AccountListFilters = { ...filters, runtime: [] }
  const counts: AccountFacetCounts = {
    enabled: { all: 0, enabled: 0, disabled: 0 },
    dispatch: { all: 0, dispatchable: 0, blocked: 0 },
    runtime: { cooldown: 0, concurrency_full: 0, probation: 0, failing: 0, auto_disabled: 0 },
  }
  for (const account of accounts) {
    const status = statuses.get(account.id)
    if (accountMatchesFilters(account, status, enabledFilters, now)) {
      counts.enabled.all += 1
      counts.enabled[account.enabled ? 'enabled' : 'disabled'] += 1
    }
    if (accountMatchesFilters(account, status, dispatchFilters, now)) {
      counts.dispatch.all += 1
      counts.dispatch[isDispatchable(status) ? 'dispatchable' : 'blocked'] += 1
    }
    if (accountMatchesFilters(account, status, runtimeFilters, now)) {
      for (const flag of accountRuntimeFlags(account, status, now)) counts.runtime[flag] += 1
    }
  }
  return counts
}

/** 模型名仅忽略首尾空白和大小写。 */
export function normalizeModelName(model: string): string {
  return model.trim().toLowerCase()
}

/** 账号能否承接这个模型：支持模型列表为空表示不限制。 */
export function accountSupportsModel(account: Pick<Account, 'supportedModels' | 'modelFilterMode'>, model: string): boolean {
  const models = account.supportedModels ?? []
  if (models.length === 0) return true
  const target = normalizeModelName(model)
  const matched = models.some((item) => normalizeModelName(item) === target)
  return account.modelFilterMode === 'denylist' ? !matched : matched
}

/** 模型筛选的候选：账号列表里出现过的模型 + 模型目录，按口径去重后排序。 */
export function modelFilterOptions(accounts: readonly Account[], catalog: readonly string[] = []): string[] {
  const seen = new Map<string, string>()
  for (const model of [...accounts.flatMap((account) => account.supportedModels ?? []), ...catalog]) {
    const trimmed = model.trim()
    if (!trimmed || trimmed.toLowerCase() === 'auto') continue
    const key = normalizeModelName(trimmed)
    if (!seen.has(key)) seen.set(key, trimmed)
  }
  return [...seen.values()].sort((a, b) => a.localeCompare(b))
}

/** "#12" 精确定位一个账号（用量明细跳过来用），其它关键字按名称、ID、地址、标签、备注模糊匹配。 */
export function matchesKeyword(account: Account, keyword: string): boolean {
  const text = keyword.trim().toLowerCase()
  if (!text) return true
  if (/^#\d+$/.test(text)) return account.id === Number(text.slice(1))
  return `${account.name} ${account.id} ${account.baseUrl} ${(account.tags ?? []).join(' ')} ${account.notes ?? ''}`
    .toLowerCase()
    .includes(text)
}

export function hasActiveFilters(filters: AccountListFilters): boolean {
  return (
    filters.keyword.trim() !== '' ||
    filters.tag !== TAG_FILTER_ALL ||
    filters.enabled !== 'all' ||
    filters.dispatch !== 'all' ||
    filters.runtime.length > 0 ||
    filters.model !== MODEL_FILTER_ALL ||
    filters.proxy !== 'all'
  )
}

/**
 * 单个账号是否满足全部筛选。多个运行状态同时选中时取交集（同时满足），
 * 对应 main credential_matches_query 里 runtime_status 逗号分隔后逐项 all 匹配的规则。
 */
export function accountMatchesFilters(
  account: Account,
  status: AccountStatus | undefined,
  filters: AccountListFilters,
  now = Date.now(),
): boolean {
  if (!matchesKeyword(account, filters.keyword)) return false
  if (filters.tag !== TAG_FILTER_ALL && !(account.tags ?? []).includes(filters.tag)) return false
  if (filters.enabled === 'enabled' && !account.enabled) return false
  if (filters.enabled === 'disabled' && account.enabled) return false
  if (filters.dispatch === 'dispatchable' && !isDispatchable(status)) return false
  if (filters.dispatch === 'blocked' && isDispatchable(status)) return false
  if (filters.runtime.length > 0) {
    const flags = accountRuntimeFlags(account, status, now)
    if (!filters.runtime.every((flag) => flags.has(flag))) return false
  }
  if (filters.model === MODEL_FILTER_UNRESTRICTED) {
    if ((account.supportedModels ?? []).length > 0) return false
  } else if (filters.model !== MODEL_FILTER_ALL && !accountSupportsModel(account, filters.model)) {
    return false
  }
  const hasProxy = Boolean(account.proxyUrl?.trim())
  if (filters.proxy === 'with_proxy' && !hasProxy) return false
  if (filters.proxy === 'without_proxy' && hasProxy) return false
  return true
}

export function filterAccounts(
  accounts: readonly Account[],
  statuses: StatusMap,
  filters: AccountListFilters,
  now = Date.now(),
): Account[] {
  return accounts.filter((account) => accountMatchesFilters(account, statuses.get(account.id), filters, now))
}

function timeValue(value: string | undefined): number | null {
  if (!value) return null
  const parsed = Date.parse(value)
  return Number.isNaN(parsed) ? null : parsed
}

function sortValue(account: Account, status: AccountStatus | undefined, sortBy: AccountSortBy): number | string | null {
  switch (sortBy) {
    case 'priority': return account.priority
    case 'name': return account.name
    case 'id': return account.id
    case 'created_at': return timeValue(account.createdAt)
    case 'updated_at': return timeValue(account.updatedAt)
    case 'dispatched': return status?.dispatchedTotal ?? null
    case 'in_flight': return status?.inFlight ?? null
    case 'error_rate': return status?.quality?.recentErrorRate ?? null
    case 'ttft': return status?.quality?.ttftEwmaMs ?? null
    case 'cost_multiplier': return account.costMultiplier
    default: return null
  }
}

/** 稳定排序；没有数据的账号（比如还没有质量样本）不论升降序都排在最后，同值按 ID 升序。 */
export function sortAccounts(accounts: readonly Account[], statuses: StatusMap, sortBy: AccountSortBy, order: SortOrder): Account[] {
  if (sortBy === 'default') return [...accounts]
  const direction = order === 'asc' ? 1 : -1
  return accounts
    .map((account) => ({ account, value: sortValue(account, statuses.get(account.id), sortBy) }))
    .sort((left, right) => {
      if (left.value === null && right.value === null) return left.account.id - right.account.id
      if (left.value === null) return 1
      if (right.value === null) return -1
      const compared = typeof left.value === 'string' || typeof right.value === 'string'
        ? String(left.value).localeCompare(String(right.value))
        : left.value - right.value
      return compared === 0 ? left.account.id - right.account.id : compared * direction
    })
    .map((item) => item.account)
}

const SKIPPED_REASON_LABELS: Record<string, string> = {
  accounts_disabled: '账号池已关闭',
  disabled: '已停用',
  auto_disabled: '自动禁用',
  cooldown: '冷却中',
  pool_concurrency_full: '并发已满',
  global_concurrency_full: '全局并发已满',
  coordinator_state_invalid: '调度协调状态异常',
}

/** /accounts/status 的 skippedReason 转成中文；未知值原样显示。 */
export function skippedReasonLabel(reason: string | null | undefined): string {
  if (!reason) return '不可调度'
  return SKIPPED_REASON_LABELS[reason] ?? reason
}

export function paginate<T>(items: readonly T[], page: number, pageSize: number): { items: T[]; page: number; pageCount: number } {
  const pageCount = Math.max(1, Math.ceil(items.length / pageSize))
  const current = Math.min(Math.max(1, page), pageCount)
  return { items: items.slice((current - 1) * pageSize, current * pageSize), page: current, pageCount }
}

export type SelectionState = 'none' | 'some' | 'all'

/** 当前页的勾选状态：全选框据此显示勾、半选或空。 */
export function pageSelectionState(pageIds: readonly number[], selected: ReadonlySet<number>): SelectionState {
  const count = pageIds.filter((id) => selected.has(id)).length
  if (count === 0) return 'none'
  return count === pageIds.length ? 'all' : 'some'
}

/** 全选当前页：当前页已全选时取消当前页，否则把当前页全部加入；别的页已选的保持不变。 */
export function toggleSelectPage(pageIds: readonly number[], selected: ReadonlySet<number>): Set<number> {
  const next = new Set(selected)
  if (pageSelectionState(pageIds, selected) === 'all') pageIds.forEach((id) => next.delete(id))
  else pageIds.forEach((id) => next.add(id))
  return next
}

export function toggleSelectOne(id: number, selected: ReadonlySet<number>): Set<number> {
  const next = new Set(selected)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  return next
}

/** 账号被删除或列表刷新后，去掉已不存在的勾选；没有变化时返回原对象，避免无谓的重渲染。 */
export function pruneSelection(selected: ReadonlySet<number>, existingIds: Iterable<number>): ReadonlySet<number> {
  const existing = new Set(existingIds)
  const kept = [...selected].filter((id) => existing.has(id))
  return kept.length === selected.size ? selected : new Set(kept)
}
