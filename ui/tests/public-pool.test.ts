// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  PUBLIC_STATUS_TABS,
  publicAccountLabel,
  publicAttentionFlags,
  publicStatusMeta,
} from '../src/features/public-pool/public-pool-model.ts'

const account = (extra: Record<string, unknown> = {}) => ({
  id: 7,
  label: 'al***@gmail.com',
  endpoint: 'ide',
  fast: 0,
  priority: 0,
  disabled: false,
  isCurrent: false,
  cooledDown: false,
  cooldownRemainingSecs: 0,
  rateLimited: false,
  rateLimitRemainingSecs: 0,
  inFlightRequests: 0,
  maxConcurrentRequests: 4,
  rpm: 0,
  recentSchedulerSelectionCount60s: 0,
  successCount: 0,
  schedulerSelectionCount: 0,
  inProbation: false,
  probationRemainingSecs: 0,
  warmupRemaining: 0,
  transientFailureStreak: 0,
  kiroMeteringUsage: 0,
  createdAt: null,
  lastUsedAt: null,
  ...extra,
}) as Parameters<typeof publicStatusMeta>[0]

test('accounts without a masked email go by their id', () => {
  assert.equal(publicAccountLabel({ id: 3, label: 'ab***@x.com' }), 'ab***@x.com')
  assert.equal(publicAccountLabel({ id: 3 }), '账号 #3')
  assert.equal(publicAccountLabel({ id: 3, label: '  ' }), '账号 #3')
})

test('the leading state follows the account page rules', () => {
  assert.equal(publicStatusMeta(account()).kind, 'normal')
  assert.equal(publicStatusMeta(account({ disabled: true })).kind, 'disabled')
  assert.equal(publicStatusMeta(account({ disabled: true, disabledReason: 'QuotaExceeded' })).label, '额度用尽')
  assert.equal(publicStatusMeta(account({ disabled: true, disabledReason: 'QuotaExceeded' })).kind, 'error')
  const cooling = publicStatusMeta(account({
    cooledDown: true,
    cooldownRemainingSecs: 30,
    cooldowns: [{ model: 'claude-opus-5-5', global: false, remainingSecs: 30 }],
  }))
  assert.equal(cooling.kind, 'cooldown')
  assert.equal(cooling.label, '模型冷却')
  assert.equal(publicStatusMeta(account({ inFlightRequests: 4 })).kind, 'concurrency_full')
})

test('only attention markers show, never settings the public data does not carry', () => {
  assert.deepEqual(publicAttentionFlags(account()), [])
  const flags = publicAttentionFlags(account({ isCurrent: true, transientFailureStreak: 2, warmupRemaining: 1 }))
  assert.deepEqual(flags.map((flag) => flag.key), ['current', 'errors', 'warmup'])
  // The error marker has no tooltip: the raw error is not public.
  assert.equal(flags[1].title, undefined)
})

test('status tabs match the account page', () => {
  assert.deepEqual(PUBLIC_STATUS_TABS.map((tab) => tab.label), ['全部', '正常', '冷却', '额度用尽', '账号异常', '已禁用'])
})
