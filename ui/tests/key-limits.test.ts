// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  MAX_LIMITS_PER_KEY,
  formatLimitValue,
  limitSummary,
  nextDraft,
  parseDrafts,
  toDrafts,
  usageView,
} from '../src/features/security/key-limits.ts'

test('drafts round-trip saved limits and parse back to the same values', () => {
  const limits = [
    { metric: 'cost_usd' as const, period: 'daily' as const, limit: 5 },
    { metric: 'credits' as const, period: 'monthly' as const, limit: 1200.5 },
  ]
  assert.deepEqual(parseDrafts(toDrafts(limits)), { limits, error: null })
  assert.deepEqual(toDrafts(undefined), [])
})

test('parsing rejects what the server rejects', () => {
  const draft = (metric: 'cost_usd' | 'credits' | 'tokens', period: 'daily' | 'monthly' | 'total', value: string) => ({ metric, period, value })
  assert.match(parseDrafts([draft('cost_usd', 'daily', '')]).error ?? '', /大于 0/)
  assert.match(parseDrafts([draft('cost_usd', 'daily', '0')]).error ?? '', /大于 0/)
  assert.match(parseDrafts([draft('credits', 'daily', 'abc')]).error ?? '', /大于 0/)
  assert.match(parseDrafts([draft('tokens', 'daily', '10.5')]).error ?? '', /整数/)
  assert.match(parseDrafts([draft('credits', 'daily', '1'), draft('credits', 'daily', '2')]).error ?? '', /重复/)
  // Same metric on different periods is fine.
  assert.equal(parseDrafts([draft('credits', 'daily', '1'), draft('credits', 'monthly', '20')]).error, null)
})

test('add-limit offers each metric + period pair once, then stops', () => {
  let drafts: ReturnType<typeof toDrafts> = []
  for (let i = 0; i < MAX_LIMITS_PER_KEY; i += 1) {
    const next = nextDraft(drafts)
    assert.ok(next, `pair ${i}`)
    drafts = [...drafts, { ...next!, value: '1' }]
  }
  assert.equal(nextDraft(drafts), null)
  assert.equal(new Set(drafts.map((draft) => `${draft.metric}:${draft.period}`)).size, MAX_LIMITS_PER_KEY)
  assert.equal(parseDrafts(drafts).error, null)
})

test('values format per metric', () => {
  assert.equal(formatLimitValue('cost_usd', 12.5), '$12.5')
  assert.equal(formatLimitValue('cost_usd', 0.000123), '$0.000123')
  assert.equal(formatLimitValue('credits', 1234.5), '1,234.5')
  assert.equal(formatLimitValue('tokens', 1500000), '1,500,000')
  assert.equal(formatLimitValue('tokens', Number.NaN), '-')
  assert.equal(limitSummary({ metric: 'cost_usd', period: 'daily', limit: 5 }), '每天 计费金额 $5')
})

test('usage bars turn amber at 80% and red at the limit', () => {
  const status = (used: number, limit: number) => ({ metric: 'credits' as const, period: 'daily' as const, limit, used, exceeded: used >= limit })
  assert.deepEqual(usageView(status(50, 100)), { percent: 50, tone: 'success' })
  assert.deepEqual(usageView(status(85, 100)), { percent: 85, tone: 'warning' })
  assert.deepEqual(usageView(status(130, 100)), { percent: 100, tone: 'error' })
  assert.deepEqual(usageView(status(5, 0)), { percent: 0, tone: 'error' })
})
