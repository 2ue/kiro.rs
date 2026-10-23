// Run with: node --test ui/tests/   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  accountRuleGroups,
  assignPolicy,
  buildAccountRows,
  filterRows,
  initialBatchPolicy,
  policyKey,
  selectByClick,
} from '../src/features/route-scenarios/account-rules.ts'

const deny = (...routeRules: string[]) => ({ routeMode: 'deny_list' as const, routeRules })
const allow = (...routeRules: string[]) => ({ routeMode: 'allow_list' as const, routeRules })
const accounts = Array.from({ length: 60 }, (_, i) => ({ id: i + 1, label: `user${i + 1}@example.com`, routeMode: 'allow_all' as const, routeRules: [] }))

test('assignPolicy writes one rule to every target and replaces earlier rules', () => {
  const value = [{ id: 3, ...allow('/v1') }, { id: 70, ...deny('/ha') }]
  const ids = accounts.slice(10).map((account) => account.id) // 50 accounts, #11..#60
  const next = assignPolicy(value, ids, deny('/cc'))
  assert.equal(next.length, 52) // #3, the deleted #70, and the 50 targets
  assert.deepEqual(next.find((entry) => entry.id === 3), { id: 3, ...allow('/v1') })
  assert.deepEqual(next.find((entry) => entry.id === 11), { id: 11, ...deny('/cc') })
  // Ids stay sorted and unique.
  assert.deepEqual(next.map((entry) => entry.id), [...new Set(next.map((entry) => entry.id))].sort((a, b) => a - b))

  const overwritten = assignPolicy(next, [3, 11], allow('/ha'))
  assert.deepEqual(overwritten.find((entry) => entry.id === 3), { id: 3, ...allow('/ha') })
  assert.deepEqual(overwritten.find((entry) => entry.id === 11), { id: 11, ...allow('/ha') })
  assert.equal(overwritten.length, 52)
})

test('assignPolicy with "unrestricted" removes the accounts and does not share route arrays', () => {
  const rule = deny('/cc')
  const next = assignPolicy([], [1, 2], rule)
  rule.routeRules.push('/ha')
  next[0].routeRules.push('/na')
  assert.deepEqual(next[1].routeRules, ['/cc'])
  assert.deepEqual(assignPolicy(next, [1, 2, 99], { routeMode: 'allow_all', routeRules: [] }), [])
})

test('rule groups ignore route order and case', () => {
  assert.equal(policyKey(deny('/cc', '/ha')), policyKey(deny('/HA', '/cc')))
  assert.notEqual(policyKey(deny('/cc')), policyKey(allow('/cc')))
  const groups = accountRuleGroups([{ id: 1, ...deny('/cc', '/ha') }, { id: 2, ...deny('/ha', '/cc') }, { id: 3, ...allow('/v1') }])
  assert.deepEqual(groups.map((group) => group.count), [2, 1])
})

test('rows list every account, deleted ones with a rule first', () => {
  const rows = buildAccountRows(accounts.slice(0, 3), [{ id: 2, ...deny('/cc') }, { id: 99, ...deny('/ha') }])
  assert.deepEqual(rows.map((row) => [row.id, row.missing, Boolean(row.rule)]), [[99, true, true], [1, false, false], [2, false, true], [3, false, false]])
})

test('filters combine rule state and search', () => {
  const rows = buildAccountRows(accounts, [{ id: 2, ...deny('/cc', '/ha') }, { id: 5, ...deny('/ha', '/cc') }, { id: 7, ...allow('/v1') }])
  assert.equal(filterRows(rows, 'all', '').length, 60)
  assert.equal(filterRows(rows, 'set', '').length, 3)
  assert.equal(filterRows(rows, 'unset', '').length, 57)
  assert.deepEqual(filterRows(rows, `rule:${policyKey(deny('/cc', '/ha'))}`, '').map((row) => row.id), [2, 5])
  assert.deepEqual(filterRows(rows, 'all', 'user7@').map((row) => row.id), [7])
  assert.deepEqual(filterRows(rows, 'all', '仅允许').map((row) => row.id), [7])
  assert.equal(filterRows(rows, 'unset', '未设置').length, 57)
})

test('shift-click selects and clears ranges within the visible list', () => {
  const rows = buildAccountRows(accounts, [])
  let selected = new Set(rows.map((row) => row.id))
  selected = selectByClick(selected, rows, 0, null, false) // uncheck #1
  selected = selectByClick(selected, rows, 9, rows[0].id, true) // shift to #10 -> clear #1..#10
  assert.equal(selected.size, 50)
  assert.ok(!selected.has(10) && selected.has(11))
  selected = selectByClick(selected, rows, 4, rows[9].id, true) // shift back to #5 -> re-check #5..#10
  assert.equal(selected.size, 56)
  // An anchor no longer in the list falls back to a single toggle.
  assert.equal(selectByClick(new Set(), rows.slice(20), 0, 1, true).size, 1)
  // Out-of-range index changes nothing.
  assert.equal(selectByClick(new Set([1]), rows, 999, null, false).size, 1)
})

test('batch dialog starts from the shared rule, otherwise from an empty deny list', () => {
  const rows = buildAccountRows(accounts.slice(0, 3), [{ id: 1, ...deny('/cc', '/ha') }, { id: 2, ...deny('/ha', '/cc') }])
  assert.deepEqual(initialBatchPolicy(rows.slice(0, 2)), deny('/cc', '/ha'))
  assert.deepEqual(initialBatchPolicy(rows), deny())
  assert.deepEqual(initialBatchPolicy(rows.slice(2)), deny())
})
