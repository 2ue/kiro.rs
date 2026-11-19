// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  ACCOUNT_EVENT_META,
  ACCOUNT_EVENT_ORDER,
  accountEventQuery,
  accountEventView,
  summarizeAccountEvents,
  summaryHonorsWindow,
  summaryTiles,
} from '../src/features/audit/account-events.ts'

const row = (detail: unknown, extra: Record<string, unknown> = {}) => ({
  id: 1,
  createdAt: '2026-10-23T00:00:00Z',
  actor: 'system-scheduler',
  action: 'auto_disable_credential',
  objectType: 'credential',
  objectId: '7',
  success: true,
  detail,
  ...extra,
})

test('every event kind has a label and a place in the order', () => {
  assert.deepEqual([...ACCOUNT_EVENT_ORDER].sort(), Object.keys(ACCOUNT_EVENT_META).sort())
})

test('account event rows read label, reason and actor from the detail', () => {
  const view = accountEventView(row({ accountEvent: 'quota_exhausted', accountSource: 'local', accountLabel: 'a@example.com', reasonLabel: '额度耗尽' }))
  assert.deepEqual(view && { kind: view.kind, label: view.label, tone: view.tone, source: view.source, accountId: view.accountId, accountLabel: view.accountLabel, reason: view.reason, actor: view.actor }, {
    kind: 'quota_exhausted', label: '额度耗尽', tone: 'error', source: 'local', accountId: '7', accountLabel: 'a@example.com', reason: '额度耗尽', actor: '调度器',
  })
})

test('older scheduler rows fall back to accountLabel and message', () => {
  const view = accountEventView(row({ accountEvent: 'auto_disabled', accountLabel: 'old@example.com', message: '连续失败' }))
  assert.equal(view?.accountLabel, 'old@example.com')
  assert.equal(view?.reason, '连续失败')
  // External rows without an explicit source are recognised by object type.
  assert.equal(accountEventView(row({ accountEvent: 'auto_disabled' }, { objectType: 'account' }))?.source, 'account')
})

test('rows that are not account events are skipped', () => {
  assert.equal(accountEventView(row({ disabled: true })), null)
  assert.equal(accountEventView(row(null)), null)
  assert.equal(accountEventView(row('text')), null)
})

test('the account view query maps the filters and the time window', () => {
  const now = new Date('2026-10-23T12:00:00Z')
  assert.deepEqual(accountEventQuery({ range: '24', kind: '__all__', source: '__all__', account: '  ' }, 1, 20, now), {
    page: 1, limit: 20, accountEvents: true, since: '2026-10-22T12:00:00.000Z',
  })
  assert.deepEqual(accountEventQuery({ range: 'all', kind: 'quota_exhausted', source: 'external', account: ' #9 ' }, 3, 20, now), {
    page: 3, limit: 20, accountEvents: true, accountEvent: 'quota_exhausted', accountSource: 'external', account: '#9',
  })
})

test('summary tiles follow the display order and drop empty kinds', () => {
  const tiles = summaryTiles([
    { event: 'auto_enabled', events: 2, accounts: 2 },
    { event: 'quota_exhausted', events: 5, accounts: 4 },
    { event: 'manual_enabled', events: 0, accounts: 0 },
    { event: 'something_new', events: 1, accounts: 1 },
  ])
  assert.deepEqual(tiles.map((tile) => [tile.kind, tile.events, tile.accounts]), [['quota_exhausted', 5, 4], ['auto_enabled', 2, 2]])
})

test('按窗口内已加载的异动统计计数：同一账号多次只算一个账号', () => {
  const events = [
    { kind: 'cooldown_started', source: 'account', accountId: '1' },
    { kind: 'cooldown_started', source: 'account', accountId: '1' },
    { kind: 'cooldown_started', source: 'account', accountId: '2' },
    { kind: 'auto_disabled', source: 'account', accountId: '2' },
    { kind: 'auto_disabled', source: 'external', accountId: '2' },
  ]
  const rows = summarizeAccountEvents(events)
  assert.deepEqual(
    rows.sort((a, b) => a.event.localeCompare(b.event)),
    [
      { event: 'auto_disabled', events: 2, accounts: 2 },
      { event: 'cooldown_started', events: 3, accounts: 2 },
    ],
  )
  // 和汇总接口的结果走同一个展示顺序。
  assert.deepEqual(summaryTiles(rows).map((tile) => tile.kind), ['auto_disabled', 'cooldown_started'])
  assert.deepEqual(summarizeAccountEvents([]), [])
})

test('汇总响应带 until 才是按窗口统计的；老后端没有 until，要退回明细统计', () => {
  assert.equal(summaryHonorsWindow({ until: '2026-11-19T00:00:00Z' }), true)
  assert.equal(summaryHonorsWindow({}), false)
  assert.equal(summaryHonorsWindow({ until: null }), false)
  assert.equal(summaryHonorsWindow({ until: '' }), false)
  // 没有响应时不算按窗口统计；页面只在响应已返回、且没有 until 时才判定为老后端。
  assert.equal(summaryHonorsWindow(undefined), false)
})
