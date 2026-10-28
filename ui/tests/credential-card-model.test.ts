// Run with: node --test ui/tests/   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  credentialFlags,
  credentialRouteSummary,
  credentialStatusMeta,
  displayProxyUrl,
  formatRemaining,
  loadTone,
  proxyChoice,
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
  cooldowns: [],
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
  assert.deepEqual(credentialStatusMeta(healthy), { kind: 'normal', label: '正常' })
  assert.deepEqual(credentialStatusMeta({ ...healthy, disabled: true }), { kind: 'disabled', label: '禁用', title: undefined })
  assert.equal(credentialStatusMeta({ ...healthy, disabled: true, disabledReason: 'Manual' }).label, '手动禁用')
  assert.equal(credentialStatusMeta({ ...healthy, disabled: true, disabledReason: 'Manual' }).kind, 'disabled')
  const suspended = credentialStatusMeta({ ...healthy, disabled: true, disabledReason: 'TemporarilySuspended', cooledDown: true })
  assert.deepEqual(suspended, { kind: 'error', label: '账号暂停', title: 'TemporarilySuspended' })
  const cooling = credentialStatusMeta({ ...healthy, cooledDown: true, cooldownRemainingSecs: 42, rateLimited: true })
  assert.equal(cooling.kind, 'cooldown')
  assert.equal(cooling.label, '冷却')
  assert.equal(cooling.remainingSecs, 42)
  const limited = credentialStatusMeta({ ...healthy, rateLimited: true, rateLimitRemainingSecs: 9 })
  assert.equal(limited.kind, 'rate_limited')
  assert.equal(limited.remainingSecs, 9)
  const full = credentialStatusMeta({ ...healthy, inFlightRequests: 30 })
  assert.equal(full.kind, 'concurrency_full')
  assert.equal(full.usage, '30/30')
  // A limit of 0 means unlimited, so it is never full.
  assert.equal(credentialStatusMeta({ ...healthy, inFlightRequests: 99, maxConcurrentRequests: 0 }).kind, 'normal')
  assert.equal(credentialStatusMeta({ ...healthy, inProbation: true, probationRemainingSecs: 30 }).title, '剩余 30s')
})

test('cooldown, rate limit and full concurrency are waiting states, never errors', () => {
  for (const state of [
    { ...healthy, cooledDown: true, cooldownRemainingSecs: 5 },
    { ...healthy, rateLimited: true, rateLimitRemainingSecs: 5 },
    { ...healthy, inFlightRequests: 30 },
  ]) {
    assert.notEqual(credentialStatusMeta(state).kind, 'error')
  }
})

test('a cooldown scoped to some models says so; a global one does not', () => {
  const scoped = credentialStatusMeta({
    ...healthy,
    cooledDown: true,
    cooldownRemainingSecs: 12,
    cooldowns: [{ model: 'claude-sonnet-4.5', global: false, remainingSecs: 12 }],
  })
  assert.equal(scoped.label, '模型冷却')
  assert.match(scoped.title ?? '', /claude-sonnet-4\.5/)
  const global = credentialStatusMeta({
    ...healthy,
    cooledDown: true,
    cooldownRemainingSecs: 12,
    cooldowns: [{ global: true, remainingSecs: 12 }, { model: 'claude-sonnet-4.5', global: false, remainingSecs: 3 }],
  })
  assert.equal(global.label, '冷却')
})

test('remaining time reads as seconds, then minutes and seconds', () => {
  assert.equal(formatRemaining(0), '0s')
  assert.equal(formatRemaining(42), '42s')
  assert.equal(formatRemaining(245), '4m05s')
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

test('load tone warns from 70% and marks the limit as full, not as an error; an unlimited limit never warns', () => {
  assert.equal(loadTone(0, 30), undefined)
  assert.equal(loadTone(20, 30), undefined)
  assert.equal(loadTone(21, 30), 'warning')
  assert.equal(loadTone(30, 30), 'full')
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

const resource = (id: number, extra: Record<string, unknown> = {}) => ({
  id,
  name: `proxy-${id}`,
  proxyUrl: `socks5h://user:secret@10.0.0.${id}:1080`,
  hasPassword: true,
  enabled: true,
  createdAt: '2026-10-01T00:00:00Z',
  updatedAt: '2026-10-01T00:00:00Z',
  credentialCount: 1,
  ...extra,
})

test('proxy urls are shown without credentials', () => {
  assert.equal(displayProxyUrl('socks5h://user:secret@10.0.0.1:1080'), 'socks5h://10.0.0.1:1080')
  assert.equal(displayProxyUrl('http://10.0.0.1:8080'), 'http://10.0.0.1:8080')
  assert.equal(displayProxyUrl(undefined), '')
})

test('proxy choice resolves in the backend order and flags resources that block scheduling', () => {
  const resources = [resource(1), resource(2, { enabled: false })]
  const bound = proxyChoice({ resourceId: 1 }, resources)
  assert.equal(bound.title, 'proxy-1')
  assert.equal(bound.detail, 'socks5h://10.0.0.1:1080')
  assert.equal(bound.blocked, undefined)
  assert.match(proxyChoice({ resourceId: 2 }, resources).blocked ?? '', /已禁用/)
  assert.match(proxyChoice({ resourceId: 9 }, resources).blocked ?? '', /不存在/)
  // Before the list loads, an unknown resource is not reported as missing.
  assert.equal(proxyChoice({ resourceId: 9 }, undefined, { resourceName: 'hk-1' }).blocked, undefined)
  assert.equal(proxyChoice({ resourceId: 9 }, undefined, { resourceName: 'hk-1' }).title, 'hk-1')
  assert.equal(proxyChoice({ url: 'http://u:p@1.2.3.4:8080' }, resources).detail, 'http://1.2.3.4:8080')
  assert.equal(proxyChoice({ url: 'direct' }, resources, { globalProxyUrl: 'http://g:1' }).title, '直连')
  assert.equal(proxyChoice({}, resources, { globalProxyUrl: 'http://g:1' }).title, '全局代理')
  assert.equal(proxyChoice({}, resources).title, '直连')
})
