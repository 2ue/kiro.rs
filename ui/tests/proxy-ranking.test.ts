// Run with: node --test ui/tests/   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  autoPickProxyIds,
  proxyUsageById,
  rankProxies,
  rotateProxyAssignments,
  type ProxyLiveness,
} from '../src/features/proxies/proxy-ranking.ts'

const proxy = (id: number, extra: { enabled?: boolean; credentialCount?: number } = {}) => ({
  id,
  name: `p${id}`,
  proxyUrl: `http://127.0.0.1:${8000 + id}`,
  hasPassword: false,
  enabled: extra.enabled ?? true,
  createdAt: '',
  updatedAt: '',
  credentialCount: extra.credentialCount ?? 0,
})

const live = (durationMs: number): ProxyLiveness => ({ success: true, durationMs })
const dead: ProxyLiveness = { success: false, durationMs: 12000, message: 'timeout' }

test('usage counts bound accounts per proxy and splits out enabled ones', () => {
  const usage = proxyUsageById([
    { proxyResourceId: 1, disabled: false },
    { proxyResourceId: 1, disabled: true },
    { proxyResourceId: 2, disabled: true },
    { proxyResourceId: undefined, disabled: false },
    { proxyResourceId: null, disabled: false },
  ])
  assert.deepEqual(usage.get(1), { enabled: 1, total: 2 })
  assert.deepEqual(usage.get(2), { enabled: 0, total: 1 })
  assert.equal(usage.get(3), undefined)
})

test('usage-first: unused, then only disabled accounts, then fewest enabled accounts; speed breaks ties', () => {
  const resources = [proxy(1), proxy(2), proxy(3), proxy(4), proxy(5), proxy(6)]
  const liveness = new Map([[1, live(80)], [2, live(300)], [3, live(50)], [4, live(40)], [5, live(900)], [6, live(60)]])
  const usage = proxyUsageById([
    // 1: two enabled accounts
    { proxyResourceId: 1, disabled: false }, { proxyResourceId: 1, disabled: false },
    // 3: one enabled account
    { proxyResourceId: 3, disabled: false },
    // 4: only disabled accounts
    { proxyResourceId: 4, disabled: true }, { proxyResourceId: 4, disabled: true },
    // 6: only a disabled account
    { proxyResourceId: 6, disabled: true },
  ])
  const order = rankProxies(resources, liveness, usage, 'usage').map((item) => item.resource.id)
  // 2 and 5 have no accounts (2 faster); 4 and 6 only disabled ones (4 faster); then 3 (1 enabled), 1 (2 enabled).
  assert.deepEqual(order, [2, 5, 4, 6, 3, 1])
})

test('quality-first orders live proxies by speed alone', () => {
  const resources = [proxy(1), proxy(2), proxy(3)]
  const liveness = new Map([[1, live(500)], [2, live(90)], [3, live(200)]])
  const usage = proxyUsageById([{ proxyResourceId: 2, disabled: false }, { proxyResourceId: 2, disabled: false }])
  assert.deepEqual(rankProxies(resources, liveness, usage, 'quality').map((item) => item.resource.id), [2, 3, 1])
})

test('dead and disabled proxies sink to the bottom and are never auto-picked; untested ones sit after live ones', () => {
  const resources = [proxy(1), proxy(2), proxy(3, { enabled: false }), proxy(4)]
  const liveness = new Map([[1, dead], [2, live(400)], [3, live(10)]])
  const ranked = rankProxies(resources, liveness, new Map(), 'usage')
  // Among the unusable ones a disabled-but-reachable proxy sorts before an unreachable one.
  assert.deepEqual(ranked.map((item) => item.resource.id), [2, 4, 3, 1])
  assert.deepEqual(ranked.map((item) => item.usable), [true, true, false, false])
  // Only tested, live, enabled proxies count for the automatic pick.
  assert.deepEqual(autoPickProxyIds(ranked, 20), [2])
})

test('auto pick takes the first N live proxies, or all of them when there are fewer', () => {
  const resources = Array.from({ length: 25 }, (_, index) => proxy(index + 1))
  const liveness = new Map(resources.map((resource) => [resource.id, live(1000 - resource.id)]))
  const ranked = rankProxies(resources, liveness, new Map(), 'quality')
  const picked = autoPickProxyIds(ranked, 20)
  assert.equal(picked.length, 20)
  assert.deepEqual(picked.slice(0, 3), [25, 24, 23])
  assert.equal(autoPickProxyIds(ranked.slice(0, 5), 20).length, 5)
  assert.deepEqual(autoPickProxyIds(ranked, 0), [])
})

test('an untested proxy falls back to the list count for its usage', () => {
  const ranked = rankProxies([proxy(1, { credentialCount: 3 }), proxy(2)], new Map(), new Map(), 'usage')
  assert.deepEqual(ranked.map((item) => [item.resource.id, item.tier]), [[2, 0], [1, 2]])
})

test('rotation assigns proxies to accounts in order and wraps around', () => {
  const assignments = rotateProxyAssignments([10, 11, 12, 13, 14], [7, 3])
  assert.deepEqual([...assignments], [[10, 7], [11, 3], [12, 7], [13, 3], [14, 7]])
  assert.equal(rotateProxyAssignments([1, 2], []).size, 0)
})
