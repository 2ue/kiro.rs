// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  DEFAULT_ACCOUNT_FILTERS,
  MODEL_FILTER_UNRESTRICTED,
  accountFacetCounts,
  accountRuntimeFlags,
  accountSupportsModel,
  filterAccounts,
  hasActiveFilters,
  isAutoDisabledNow,
  matchesKeyword,
  modelFilterOptions,
  normalizeModelName,
  pageSelectionState,
  paginate,
  pruneSelection,
  skippedReasonLabel,
  sortAccounts,
  toggleSelectOne,
  toggleSelectPage,
} from '../src/features/accounts/account-list.ts'
import type { Account, AccountStatus } from '../src/types/api.ts'

const NOW = Date.parse('2026-11-19T12:00:00Z')

function account(id: number, patch: Partial<Account> = {}): Account {
  return {
    id,
    name: `acc-${id}`,
    baseUrl: `https://u${id}.example.com`,
    tags: [],
    costMultiplier: 1,
    overrides: {},
    authType: 'bearer',
    headerProfile: 'generic',
    appendBetaQuery: false,
    headerOverrides: {},
    wireProfile: 'default',
    tlsProfile: 'default',
    enabled: true,
    priority: 100,
    maxConcurrentRequests: 10,
    usageProjectionMode: 'pass_through',
    autoDisablePolicy: 'inherit',
    preOutputStreamRetryMode: 'inherit',
    autoDisabled: false,
    normalizeModelVersionDots: false,
    modelMappingMode: 'processed_mapping',
    modelMappingRequireMatch: false,
    modelMappingRules: [],
    supportedModels: [],
    routeMode: 'allow_all',
    routeRules: [],
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-01T00:00:00Z',
    ...patch,
  } as Account
}

function status(a: Account, patch: Partial<AccountStatus> = {}): AccountStatus {
  return {
    account: a,
    inFlight: 0,
    cooldownRemainingSecs: 0,
    transientFailureStreak: 0,
    transientFailureTtlSecs: 0,
    dispatchable: true,
    ...patch,
  }
}

const a1 = account(1, { tags: ['vip'], supportedModels: ['claude-sonnet-4-5'], priority: 10, proxyUrl: 'http://p:1' })
const a2 = account(2, { enabled: false, priority: 50, name: 'beta' })
const a3 = account(3, { autoDisabled: true, autoDisabledUntil: '2026-11-19T13:00:00Z', name: 'alpha' })
const a4 = account(4, { maxConcurrentRequests: 2, supportedModels: ['claude-opus-4.6'] })
const accounts = [a1, a2, a3, a4]
const statuses = new Map<number, AccountStatus>([
  [1, status(a1, { cooldownRemainingSecs: 30, dispatchable: false, skippedReason: 'cooldown', dispatchedTotal: 5 })],
  [2, status(a2, { dispatchable: false, skippedReason: 'disabled', dispatchedTotal: 1 })],
  [3, status(a3, { dispatchable: false, skippedReason: 'auto_disabled' })],
  [4, status(a4, { inFlight: 2, dispatchable: false, transientFailureStreak: 2, quality: { recentErrorRate: 0.4, sampleCount: 9, scoringActive: true, inProbation: true, probationRemainingSecs: 20, probationLevel: 1, recoveryProgress: 0 }, dispatchedTotal: 9 })],
])

test('自动禁用按到期时间判断，与后端 is_auto_disabled_now 一致', () => {
  assert.equal(isAutoDisabledNow(a3, NOW), true)
  assert.equal(isAutoDisabledNow({ autoDisabled: true, autoDisabledUntil: '2026-11-19T11:00:00Z' }, NOW), false)
  assert.equal(isAutoDisabledNow({ autoDisabled: true }, NOW), true)
  assert.equal(isAutoDisabledNow({ autoDisabled: false }, NOW), false)
})

test('运行状态：冷却、并发已满（按上限至少 1）、观察期、近期失败、自动禁用', () => {
  assert.deepEqual([...accountRuntimeFlags(a1, statuses.get(1), NOW)], ['cooldown'])
  assert.deepEqual([...accountRuntimeFlags(a4, statuses.get(4), NOW)].sort(), ['concurrency_full', 'failing', 'probation'])
  assert.deepEqual([...accountRuntimeFlags(a3, statuses.get(3), NOW)], ['auto_disabled'])
  // 并发上限为 0 时按 1 算：有一个在飞就算满
  assert.equal(accountRuntimeFlags(account(9, { maxConcurrentRequests: 0 }), status(account(9), { inFlight: 1 }), NOW).has('concurrency_full'), true)
})

test('分类计数覆盖启用状态、调度状况和每个运行状态；不传筛选就是全局计数', () => {
  const counts = accountFacetCounts(accounts, statuses, DEFAULT_ACCOUNT_FILTERS, NOW)
  assert.deepEqual(counts.enabled, { all: 4, enabled: 3, disabled: 1 })
  assert.deepEqual(counts.dispatch, { all: 4, dispatchable: 0, blocked: 4 })
  assert.deepEqual(counts.runtime, { cooldown: 1, concurrency_full: 1, probation: 1, failing: 1, auto_disabled: 1 })
  assert.deepEqual(accountFacetCounts(accounts, statuses), accountFacetCounts(accounts, statuses, DEFAULT_ACCOUNT_FILTERS))
})

test('分段计数随其它筛选变化：代理、关键字等普通筛选三类计数都要套用', () => {
  const counts = accountFacetCounts(accounts, statuses, { ...DEFAULT_ACCOUNT_FILTERS, proxy: 'without_proxy' }, NOW)
  // a1 配了代理被排除
  assert.deepEqual(counts.enabled, { all: 3, enabled: 2, disabled: 1 })
  assert.deepEqual(counts.dispatch, { all: 3, dispatchable: 0, blocked: 3 })
  assert.deepEqual(counts.runtime, { cooldown: 0, concurrency_full: 1, probation: 1, failing: 1, auto_disabled: 1 })
  const byKeyword = accountFacetCounts(accounts, statuses, { ...DEFAULT_ACCOUNT_FILTERS, keyword: 'alpha' }, NOW)
  assert.deepEqual(byKeyword.enabled, { all: 1, enabled: 1, disabled: 0 })
  assert.deepEqual(byKeyword.runtime, { cooldown: 0, concurrency_full: 0, probation: 0, failing: 0, auto_disabled: 1 })
})

test('分段计数口径对齐 main credential_filter_facets：启用状态去掉启用和调度筛选，调度状况只去掉自身，运行状态只去掉自身', () => {
  const b1 = account(11, { name: 'b1' })
  const b2 = account(12, { name: 'b2' })
  const b3 = account(13, { name: 'b3', enabled: false })
  const list = [b1, b2, b3]
  const map = new Map<number, AccountStatus>([
    [11, status(b1, { dispatchable: true })],
    [12, status(b2, { dispatchable: false, cooldownRemainingSecs: 10 })],
    [13, status(b3, { dispatchable: false })],
  ])
  const facets = (patch: Partial<typeof DEFAULT_ACCOUNT_FILTERS>) =>
    accountFacetCounts(list, map, { ...DEFAULT_ACCOUNT_FILTERS, ...patch }, NOW)

  // 选了“可调度”：启用状态不受调度筛选影响；调度状况不受自身影响；运行状态套用调度筛选，只剩 b1
  const dispatchable = facets({ dispatch: 'dispatchable' })
  assert.deepEqual(dispatchable.enabled, { all: 3, enabled: 2, disabled: 1 })
  assert.deepEqual(dispatchable.dispatch, { all: 3, dispatchable: 1, blocked: 2 })
  assert.equal(dispatchable.runtime.cooldown, 0)

  // 选了“停用”：启用状态不受自身影响；调度状况和运行状态都套用启用筛选
  const disabled = facets({ enabled: 'disabled' })
  assert.deepEqual(disabled.enabled, { all: 3, enabled: 2, disabled: 1 })
  assert.deepEqual(disabled.dispatch, { all: 1, dispatchable: 0, blocked: 1 })
  assert.equal(disabled.runtime.cooldown, 0)

  // 选了运行状态“冷却中”：启用状态和调度状况都套用；运行状态各项不受自身影响
  const cooling = facets({ runtime: ['cooldown'] })
  assert.deepEqual(cooling.enabled, { all: 1, enabled: 1, disabled: 0 })
  assert.deepEqual(cooling.dispatch, { all: 1, dispatchable: 0, blocked: 1 })
  assert.equal(cooling.runtime.cooldown, 1)
  assert.deepEqual(facets({ runtime: ['cooldown', 'probation'] }).runtime, facets({}).runtime)
})

test('模型口径忽略大小写、版本点号与横杠视为相同；列表为空表示不限制', () => {
  assert.equal(normalizeModelName('Claude-Sonnet-4.5'), 'claude-sonnet-4-5')
  assert.equal(accountSupportsModel(a1, 'claude-sonnet-4.5'), true)
  assert.equal(accountSupportsModel(a1, 'claude-opus-4-6'), false)
  assert.equal(accountSupportsModel(a2, 'anything'), true)
  assert.deepEqual(modelFilterOptions(accounts, ['claude-opus-4-6', 'auto', 'claude-haiku-4-5']), ['claude-haiku-4-5', 'claude-opus-4.6', 'claude-sonnet-4-5'])
})

test('关键字："#3" 精确定位，其它按名称/地址/标签模糊匹配', () => {
  assert.equal(matchesKeyword(a3, '#3'), true)
  assert.equal(matchesKeyword(account(33), '#3'), false)
  assert.equal(matchesKeyword(a1, 'VIP'), true)
  assert.equal(matchesKeyword(a1, 'u1.example'), true)
})

test('筛选：启用状态、运行状态取交集、按模型、按代理、按标签', () => {
  const ids = (filters: Partial<typeof DEFAULT_ACCOUNT_FILTERS>) =>
    filterAccounts(accounts, statuses, { ...DEFAULT_ACCOUNT_FILTERS, ...filters }, NOW).map((a) => a.id)
  assert.deepEqual(ids({}), [1, 2, 3, 4])
  assert.deepEqual(ids({ enabled: 'disabled' }), [2])
  assert.deepEqual(ids({ runtime: ['probation', 'concurrency_full'] }), [4])
  assert.deepEqual(ids({ runtime: ['probation', 'cooldown'] }), [])
  assert.deepEqual(ids({ model: 'claude-opus-4-6' }), [2, 3, 4])
  assert.deepEqual(ids({ model: MODEL_FILTER_UNRESTRICTED }), [2, 3])
  assert.deepEqual(ids({ proxy: 'with_proxy' }), [1])
  assert.deepEqual(ids({ proxy: 'without_proxy' }), [2, 3, 4])
  assert.deepEqual(ids({ tag: 'vip' }), [1])
  assert.equal(hasActiveFilters(DEFAULT_ACCOUNT_FILTERS), false)
  assert.equal(hasActiveFilters({ ...DEFAULT_ACCOUNT_FILTERS, runtime: ['cooldown'] }), true)
})

test('排序：优先级升降序、名称、调度次数；没有数据的排最后，同值按 ID', () => {
  assert.deepEqual(sortAccounts(accounts, statuses, 'priority', 'asc').map((a) => a.id), [1, 2, 3, 4])
  assert.deepEqual(sortAccounts(accounts, statuses, 'priority', 'desc').map((a) => a.id), [3, 4, 2, 1])
  assert.deepEqual(sortAccounts(accounts, statuses, 'name', 'asc').map((a) => a.id), [1, 4, 3, 2])
  assert.deepEqual(sortAccounts(accounts, statuses, 'dispatched', 'desc').map((a) => a.id), [4, 1, 2, 3])
  assert.deepEqual(sortAccounts(accounts, statuses, 'dispatched', 'asc').map((a) => a.id), [2, 1, 4, 3])
  assert.deepEqual(sortAccounts(accounts, statuses, 'error_rate', 'asc').map((a) => a.id), [4, 1, 2, 3])
  assert.deepEqual(sortAccounts(accounts, statuses, 'default', 'desc').map((a) => a.id), [1, 2, 3, 4])
})

test('不可调度原因转中文，未知原因原样显示', () => {
  assert.equal(skippedReasonLabel('pool_concurrency_full'), '并发已满')
  assert.equal(skippedReasonLabel(undefined), '不可调度')
  assert.equal(skippedReasonLabel('something_new'), 'something_new')
})

test('分页越界时落到最后一页', () => {
  const items = Array.from({ length: 45 }, (_, i) => i)
  assert.deepEqual(paginate(items, 3, 20), { items: [40, 41, 42, 43, 44], page: 3, pageCount: 3 })
  assert.equal(paginate(items, 9, 20).page, 3)
  assert.equal(paginate([], 1, 20).pageCount, 1)
})

test('全选当前页：已全选再点取消当前页，其它页的勾选保留', () => {
  const selected = new Set([1, 99])
  assert.equal(pageSelectionState([1, 2], selected), 'some')
  const all = toggleSelectPage([1, 2], selected)
  assert.deepEqual([...all].sort((a, b) => a - b), [1, 2, 99])
  assert.equal(pageSelectionState([1, 2], all), 'all')
  assert.deepEqual([...toggleSelectPage([1, 2], all)], [99])
  assert.deepEqual([...toggleSelectOne(2, new Set([1]))], [1, 2])
  assert.deepEqual([...toggleSelectOne(1, new Set([1]))], [])
})

test('删除后的账号从勾选里去掉；没有变化时返回同一个对象', () => {
  const selected = new Set([1, 2, 3])
  assert.deepEqual([...pruneSelection(selected, [1, 3, 4])], [1, 3])
  assert.equal(pruneSelection(selected, [1, 2, 3, 4]), selected)
})

test('模型黑名单筛选只排除命中项，缺省白名单保持兼容', () => {
  assert.equal(accountSupportsModel({ supportedModels: ['fable-5'], modelFilterMode: 'denylist' }, 'fable-5'), false)
  assert.equal(accountSupportsModel({ supportedModels: ['fable-5'], modelFilterMode: 'denylist' }, 'other-model'), true)
  assert.equal(accountSupportsModel({ supportedModels: [], modelFilterMode: 'denylist' }, 'fable-5'), true)
  assert.equal(accountSupportsModel({ supportedModels: ['fable-5'] }, 'other-model'), false)
})
