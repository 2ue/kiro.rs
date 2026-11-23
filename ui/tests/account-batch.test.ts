// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  BATCH_NOT_STARTED_MESSAGE,
  applySupportedModelsEdit,
  buildBatchUpdate,
  countBatchEditFields,
  markNotStartedRows,
  mergeOverrides,
  notStartedIndices,
  resetRows,
  runWithConcurrency,
  summarizeBatchRows,
  validateBatchEditDraft,
  type BatchRow,
} from '../src/features/accounts/account-batch.ts'
import type { Account } from '../src/types/api.ts'

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

const base = {
  id: 7,
  name: 'a',
  baseUrl: 'https://x.example.com',
  tags: ['old', 'keep'],
  costMultiplier: 1,
  overrides: { requestTimeoutSecs: 120, usageProjectionUpliftPercent: 30 },
  enabled: true,
  priority: 100,
  maxConcurrentRequests: 10,
  usageProjectionMode: 'pass_through',
  autoDisablePolicy: 'inherit',
  preOutputStreamRetryMode: 'inherit',
  supportedModels: ['claude-sonnet-4-5'],
  routeMode: 'allow_all',
  routeRules: [],
  proxyUrl: 'http://***:***@proxy:8080/',
} as unknown as Account

test('并发执行器同时最多跑 limit 个，全部完成', async () => {
  let running = 0
  let peak = 0
  const done: number[] = []
  const result = await runWithConcurrency(Array.from({ length: 10 }, (_, i) => i), async (item) => {
    running += 1
    peak = Math.max(peak, running)
    await sleep(5)
    running -= 1
    done.push(item)
  }, { concurrency: 4 })
  assert.equal(peak, 4)
  assert.equal(done.length, 10)
  assert.deepEqual(result, { stopped: false, started: 10 })
})

test('取消后不再开始新任务，已开始的照常完成', async () => {
  let stop = false
  const finished: number[] = []
  const result = await runWithConcurrency(Array.from({ length: 10 }, (_, i) => i), async (item) => {
    if (item === 1) stop = true
    await sleep(5)
    finished.push(item)
  }, { concurrency: 2, shouldStop: () => stop })
  assert.equal(result.stopped, true)
  assert.ok(result.started < 10)
  assert.equal(finished.length, result.started)
})

test('结果统计', () => {
  assert.deepEqual(
    summarizeBatchRows([{ status: 'success' }, { status: 'failed' }, { status: 'skipped' }, { status: 'pending' }, { status: 'running' }]),
    { total: 5, pending: 1, running: 1, success: 1, failed: 1, skipped: 1, finished: 3 },
  )
})

test('overrides 整体替换前先合并现有值：改的键覆盖，null 删除键恢复全局，未知键丢弃', () => {
  const merged = mergeOverrides(
    { requestTimeoutSecs: 120, usageProjectionUpliftPercent: 30, streamIdleTimeoutSecs: null, bogus: 1 } as never,
    { usageProjectionUpliftPercent: null, usageProjectionCostFloorMarginPercent: 20 },
  )
  assert.deepEqual(merged, { requestTimeoutSecs: 120, usageProjectionCostFloorMarginPercent: 20 })
  assert.deepEqual(mergeOverrides(undefined, { usageProjectionCostFloorEnabled: false }), { usageProjectionCostFloorEnabled: false })
})

test('批量修改只带变化的字段；启停单独返回走启停接口', () => {
  const plan = buildBatchUpdate(base, {
    priority: 100,
    maxConcurrentRequests: 20,
    enabled: false,
    tags: { add: ['new', 'keep'], remove: ['old'] },
    overrides: { usageProjectionCostFloorEnabled: true },
  })
  assert.deepEqual(plan.update, {
    maxConcurrentRequests: 20,
    tags: ['keep', 'new'],
    overrides: { requestTimeoutSecs: 120, usageProjectionUpliftPercent: 30, usageProjectionCostFloorEnabled: true },
  })
  assert.equal(plan.enabled, false)
  assert.equal(plan.changes.length, 4)
})

test('没有任何变化时 changes 为空，调用方跳过', () => {
  const plan = buildBatchUpdate(base, {
    priority: 100,
    enabled: true,
    tags: { add: ['keep'], remove: ['missing'] },
    overrides: { requestTimeoutSecs: 120 },
    supportedModels: { mode: 'append', models: ['CLAUDE-SONNET-4-5'] },
    route: { routeMode: 'allow_all', routeRules: ['/ignored'] },
  })
  assert.deepEqual(plan, { update: {}, enabled: undefined, changes: [], warnings: [] })
})

test('代理：清空发 null（没有代理的账号跳过），设置只在地址变化时发送', () => {
  assert.deepEqual(buildBatchUpdate(base, { proxy: { mode: 'clear', url: '' } }).update, { proxyUrl: null })
  assert.deepEqual(buildBatchUpdate({ ...base, proxyUrl: null }, { proxy: { mode: 'clear', url: '' } }).update, {})
  assert.deepEqual(buildBatchUpdate(base, { proxy: { mode: 'set', url: ' socks5://h:1 ' } }).update, { proxyUrl: 'socks5://h:1' })
})

test('路由：不限制时清空规则；支持模型四种操作，比较忽略大小写', () => {
  assert.deepEqual(buildBatchUpdate(base, { route: { routeMode: 'deny_list', routeRules: ['/cc'] } }).update, { routeMode: 'deny_list', routeRules: ['/cc'] })
  assert.deepEqual(applySupportedModelsEdit(['a', 'B'], { mode: 'append', models: ['b', 'c'] }), ['a', 'B', 'c'])
  assert.deepEqual(applySupportedModelsEdit(['a', 'B'], { mode: 'remove', models: ['b'] }), ['a'])
  assert.deepEqual(applySupportedModelsEdit(['a'], { mode: 'replace', models: ['x', 'X'] }), ['x'])
  assert.deepEqual(applySupportedModelsEdit(['a'], { mode: 'clear', models: ['x'] }), [])
  assert.deepEqual(buildBatchUpdate(base, { supportedModels: { mode: 'clear', models: [] } }).update, { supportedModels: [] })
})

test('批量标签：已有标签不截断不限量；达到 32 个上限时新标签不加入并给出提示，先移除腾出的名额可用', () => {
  const long = 'L'.repeat(80)
  const many = Array.from({ length: 33 }, (_, i) => `t${i}`)
  const full = { ...base, tags: [long, ...many] } as Account
  const blocked = buildBatchUpdate(full, { tags: { add: ['new'], remove: [] } })
  assert.deepEqual(blocked.update, {})
  assert.equal(blocked.changes.length, 0)
  assert.match(blocked.warnings[0], /32 个上限.*new/)
  // 已有的长标签和超过 32 个的部分都保留，移除也按原值精确匹配
  const removed = buildBatchUpdate(full, { tags: { add: [], remove: [long] } })
  assert.deepEqual(removed.update.tags, many)
  assert.deepEqual(removed.warnings, [])
  const swapped = buildBatchUpdate({ ...base, tags: many.slice(0, 32) } as Account, { tags: { add: ['new'], remove: ['t0'] } })
  assert.deepEqual(swapped.update.tags, [...many.slice(1, 32), 'new'])
  assert.deepEqual(swapped.warnings, [])
})

test('停止后没开始的行标为“跳过：已停止”，继续剩余只跑这些行，重置后回到等待', () => {
  const rows: BatchRow[] = [
    { id: 1, label: 'a', status: 'success', message: '已删除' },
    { id: 2, label: 'b', status: 'failed', message: '500' },
    { id: 3, label: 'c', status: 'pending' },
    { id: 4, label: 'd', status: 'pending' },
  ]
  const marked = markNotStartedRows(rows, [0, 1, 2, 3], new Set([0, 1]))
  assert.deepEqual(marked.slice(2).map((row) => [row.status, row.message, row.notStarted]), [
    ['skipped', BATCH_NOT_STARTED_MESSAGE, true],
    ['skipped', BATCH_NOT_STARTED_MESSAGE, true],
  ])
  assert.deepEqual(marked.slice(0, 2), rows.slice(0, 2))
  assert.deepEqual(notStartedIndices(marked), [2, 3])
  assert.equal(summarizeBatchRows(marked).skipped, 2)
  // 只重试失败时，不属于本轮的未开始行保持原样，仍可继续剩余
  const retried = markNotStartedRows(resetRows(marked, [1]), [1], new Set([1]))
  assert.deepEqual(notStartedIndices(retried), [2, 3])
  const resumed = resetRows(marked, notStartedIndices(marked))
  assert.deepEqual(resumed.slice(2), [{ id: 3, label: 'c', status: 'pending' }, { id: 4, label: 'd', status: 'pending' }])
  assert.deepEqual(notStartedIndices(resumed), [])
})

test('批量设置代理不能带 *** 占位，提示说明原因', () => {
  assert.match(validateBatchEditDraft({ proxy: { mode: 'set', url: 'http://***:***@1.2.3.4:8080/' } })!, /不能带 \*\*\*.*各不相同/)
  assert.match(validateBatchEditDraft({ proxy: { mode: 'set', url: 'socks5://user:***@h:1' } })!, /\*\*\*/)
  assert.equal(validateBatchEditDraft({ proxy: { mode: 'set', url: 'http://u:p@1.2.3.4:8080' } }), null)
  assert.equal(validateBatchEditDraft({ proxy: { mode: 'clear', url: 'http://***:***@h:1' } }), null)
})

test('提交前校验', () => {
  assert.equal(validateBatchEditDraft({}), null)
  assert.match(validateBatchEditDraft({ maxConcurrentRequests: 0 })!, /1 到 10000/)
  assert.match(validateBatchEditDraft({ costMultiplier: 0 })!, /大于 0/)
  assert.match(validateBatchEditDraft({ priority: 1.5 })!, /整数/)
  assert.match(validateBatchEditDraft({ route: { routeMode: 'allow_list', routeRules: [] } })!, /至少需要一条路由/)
  assert.match(validateBatchEditDraft({ proxy: { mode: 'set', url: 'ftp://x' } })!, /http/)
  assert.match(validateBatchEditDraft({ tags: { add: [], remove: [] } })!, /标签/)
  assert.match(validateBatchEditDraft({ supportedModels: { mode: 'replace', models: [] } })!, /清空/)
  assert.match(validateBatchEditDraft({ overrides: { requestTimeoutSecs: -1 } })!, /非负整数/)
  assert.equal(countBatchEditFields({ priority: 1, overrides: { requestTimeoutSecs: 1, streamIdleTimeoutSecs: null } }), 3)
})

test('模型筛选切换模式要求替换或清空，追加不反转旧名单', () => {
  const account = { ...base, modelFilterMode: 'allowlist', supportedModels: ['old-model'] } as Account
  assert.throws(() => buildBatchUpdate(account, { supportedModels: { mode: 'append', modelFilterMode: 'denylist', models: ['fable-5'] } }), /切换白名单/)
  const replacement = buildBatchUpdate(account, { supportedModels: { mode: 'replace', modelFilterMode: 'denylist', models: ['fable-5'] } })
  assert.equal(replacement.update.modelFilterMode, 'denylist')
  assert.deepEqual(replacement.update.supportedModels, ['fable-5'])
  const sameMode = buildBatchUpdate({ ...account, modelFilterMode: 'denylist' }, { supportedModels: { mode: 'append', modelFilterMode: 'denylist', models: ['fable-5'] } })
  assert.deepEqual(sameMode.update.supportedModels, ['old-model', 'fable-5'])
})
