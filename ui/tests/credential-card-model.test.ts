// Run with: node --test ui/tests/   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  credentialFlags,
  credentialRouteSummary,
  credentialStatusMeta,
  loadTone,
  quotaBarModel,
  quotaReading,
  subscriptionBadgeMeta,
  successRate,
} from '../src/features/credentials/credential-utils.ts'
import { formatDateTime } from '../src/lib/format.ts'

const healthy = {
  disabled: false,
  disabledReason: undefined,
  cooledDown: false,
  cooldownRemainingSecs: 0,
  cooldownReason: undefined,
  rateLimited: false,
  rateLimitRemainingSecs: 0,
  inFlightRequests: 0,
  maxConcurrentRequests: 30,
  inProbation: false,
  probationRemainingSecs: 0,
}

const plain = {
  isCurrent: false,
  disabled: false,
  transientFailureStreak: 0,
  lastErrorReason: undefined,
  warmupRemaining: 0,
  routeMode: 'allow_all' as const,
  routeRules: [],
  hasProxy: false,
  effectiveProxySource: 'none' as const,
  proxyResourceName: undefined,
  supportedModels: [],
  rateLimitAutoDisableEnabled: true,
  hasProfileArn: false,
}

test('the leading status is the most blocking condition', () => {
  assert.deepEqual(credentialStatusMeta(healthy), { label: '正常', tone: 'success' })
  assert.deepEqual(credentialStatusMeta({ ...healthy, disabled: true }), { label: '禁用', tone: 'neutral', title: undefined })
  assert.equal(credentialStatusMeta({ ...healthy, disabled: true, disabledReason: 'Manual' }).label, '手动禁用')
  assert.equal(credentialStatusMeta({ ...healthy, disabled: true, disabledReason: 'Manual' }).tone, 'neutral')
  const suspended = credentialStatusMeta({ ...healthy, disabled: true, disabledReason: 'TemporarilySuspended', cooledDown: true })
  assert.deepEqual(suspended, { label: '账号暂停', tone: 'error', title: 'TemporarilySuspended' })
  assert.equal(credentialStatusMeta({ ...healthy, cooledDown: true, cooldownRemainingSecs: 42, rateLimited: true }).label, '冷却 42s')
  assert.equal(credentialStatusMeta({ ...healthy, rateLimited: true, rateLimitRemainingSecs: 9 }).label, '限流 9s')
  assert.equal(credentialStatusMeta({ ...healthy, inFlightRequests: 30 }).label, '并发已满')
  // A limit of 0 means unlimited, so it is never full.
  assert.equal(credentialStatusMeta({ ...healthy, inFlightRequests: 99, maxConcurrentRequests: 0 }).label, '正常')
  assert.equal(credentialStatusMeta({ ...healthy, inProbation: true, probationRemainingSecs: 30 }).title, '剩余 30s')
})

test('flags put things that need a look first and standing settings after', () => {
  assert.deepEqual(credentialFlags(plain), [])
  const flags = credentialFlags({
    ...plain,
    isCurrent: true,
    transientFailureStreak: 3,
    lastErrorReason: 'timeout',
    warmupRemaining: 2,
    routeMode: 'deny_list',
    routeRules: ['/cc'],
    hasProxy: true,
    effectiveProxySource: 'resource',
    proxyResourceName: 'hk-1',
    supportedModels: ['claude-sonnet-4.5', 'claude-haiku-4.5'],
    rateLimitAutoDisableEnabled: false,
    hasProfileArn: true,
  })
  assert.deepEqual(flags.map((f) => `${f.kind}:${f.key}`), [
    'attention:current',
    'attention:errors',
    'attention:warmup',
    'config:route',
    'config:proxy',
    'config:models',
    'config:rate-limit',
    'config:profile-arn',
  ])
  assert.equal(flags.find((f) => f.key === 'errors')?.title, 'timeout')
  assert.equal(flags.find((f) => f.key === 'proxy')?.title, '代理：代理资源：hk-1')
  assert.equal(flags.find((f) => f.key === 'models')?.count, 2)
  // A disabled account's error streak is history, not something to act on.
  assert.equal(credentialFlags({ ...plain, disabled: true, transientFailureStreak: 5 }).length, 0)
})

test('route summary only appears when the route policy restricts something', () => {
  assert.equal(credentialRouteSummary('allow_all', ['/cc']), undefined)
  assert.equal(credentialRouteSummary('deny_list', []), undefined)
  assert.equal(credentialRouteSummary('allow_list', []), '不承接任何入口')
  assert.equal(credentialRouteSummary('allow_list', ['/cc', '/v1']), '仅 /cc, /v1')
  assert.equal(credentialRouteSummary('deny_list', ['/a', '/b', '/c']), '排除 /a, /b 等 3 条')
})

test('load tone warns from 70% and turns red at the limit; an unlimited limit never warns', () => {
  assert.equal(loadTone(0, 30), undefined)
  assert.equal(loadTone(20, 30), undefined)
  assert.equal(loadTone(21, 30), 'warning')
  assert.equal(loadTone(30, 30), 'error')
  assert.equal(loadTone(500, 0), undefined)
})

test('subscription meta carries the plan key so each tier gets its own colour', () => {
  assert.equal(subscriptionBadgeMeta({ subscriptionTitle: 'KIRO FREE', accountInfo: undefined }).key, 'free')
  assert.equal(subscriptionBadgeMeta({ subscriptionTitle: 'Kiro Pro+', accountInfo: undefined }).key, 'pro_plus')
  assert.equal(subscriptionBadgeMeta({ subscriptionTitle: 'KIRO POWER', accountInfo: undefined }).key, 'power')
  assert.equal(subscriptionBadgeMeta({ subscriptionTitle: undefined, accountInfo: undefined }).key, 'unknown')
})

test('exact time keeps seconds and drops the year only for this year', () => {
  assert.equal(formatDateTime(null), '-')
  assert.equal(formatDateTime('not a date'), '-')
  const now = new Date()
  const thisYear = new Date(now.getFullYear(), 0, 5, 9, 7, 3)
  assert.equal(formatDateTime(thisYear.toISOString()), '01/05 09:07:03')
  assert.equal(formatDateTime(new Date(2020, 0, 5, 9, 7, 3).getTime()), '2020/01/05 09:07:03')
})

test('quota reads as one line and only brackets the overage when there is some', () => {
  const read = (currentUsage: number, usageLimit: number, currentOverages = 0) =>
    quotaReading({ currentUsage, usageLimit }, quotaBarModel({ currentUsage, usageLimit, currentOverages }))
  assert.deepEqual(read(16.4, 50), { percent: '32.8%', amount: '16.4/50' })
  assert.deepEqual(read(500, 1000), { percent: '50%', amount: '500/1,000' })
  assert.deepEqual(read(1000, 1000), { percent: '100%', amount: '1,000/1,000' })
  // Overage reported either as usage past the limit or separately with usage held at the limit.
  assert.deepEqual(read(1345, 1000), { percent: '100%（+34.5%）', amount: '1,000(+345)/1,000' })
  assert.deepEqual(read(1000, 1000, 345), { percent: '100%（+34.5%）', amount: '1,000(+345)/1,000' })
  assert.equal(read(10, 0), undefined)
  assert.equal(quotaReading(undefined, quotaBarModel(undefined)), undefined)
})

test('success rate compares successes with dispatches and only colours a meaningful sample', () => {
  assert.deepEqual(successRate(0, 0), { dispatched: 0 })
  assert.deepEqual(successRate(2800, 3000), { dispatched: 3000, percent: (2800 / 3000) * 100, tone: 'warning' })
  assert.equal(successRate(2990, 3000).tone, 'success')
  assert.equal(successRate(420, 600).tone, 'error')
  // A handful of dispatches is too few to judge.
  assert.equal(successRate(3, 5).tone, undefined)
  // Older stats with fewer dispatches than successes read as fully successful.
  assert.deepEqual(successRate(83, 80), { dispatched: 83, percent: 100, tone: 'success' })
})
