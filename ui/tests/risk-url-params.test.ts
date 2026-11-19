// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { accountRiskLink, riskInitialFromSearch } from '../src/features/account-risk/risk-url-params.ts'

const parse = (query: string) => riskInitialFromSearch(new URLSearchParams(query))

test('没有参数时用最近 24 小时、全部账号', () => {
  assert.deepEqual(parse(''), { windowKey: 'last24h', accountId: undefined, since: undefined, until: undefined })
})

test('读取账号 ID（也认 account、#12），非法值忽略', () => {
  assert.equal(parse('accountId=12').accountId, 12)
  assert.equal(parse('account=%2312').accountId, 12)
  assert.equal(parse('accountId=abc').accountId, undefined)
  assert.equal(parse('accountId=0').accountId, undefined)
})

test('时间窗口：认已知窗口；只给 since/until 时按自定义；非自定义窗口忽略 since/until', () => {
  assert.equal(parse('window=last7d').windowKey, 'last7d')
  assert.equal(parse('window=bogus').windowKey, 'last24h')
  const custom = parse('since=2026-11-18T00:00:00Z&until=2026-11-19T00:00:00Z')
  assert.equal(custom.windowKey, 'custom')
  assert.equal(custom.since?.toISOString(), '2026-11-18T00:00:00.000Z')
  const swapped = parse('window=custom&from=2026-11-19T00:00:00Z&to=2026-11-18T00:00:00Z')
  assert.ok(swapped.since! < swapped.until!)
  assert.deepEqual(parse('window=today&since=2026-11-18T00:00:00Z'), { windowKey: 'today', accountId: undefined, since: undefined, until: undefined })
})

const NOW = new Date('2026-11-19T12:00:00Z')
const at = (query: string) => {
  const result = riskInitialFromSearch(new URLSearchParams(query), NOW)
  return { windowKey: result.windowKey, since: result.since?.toISOString(), until: result.until?.toISOString() }
}

test('自定义窗口只给 until：since = until - 24h', () => {
  assert.deepEqual(at('until=2026-11-10T08:00:00Z'), { windowKey: 'custom', since: '2026-11-09T08:00:00.000Z', until: '2026-11-10T08:00:00.000Z' })
  assert.deepEqual(at('window=custom&to=2026-11-10T08:00:00Z'), { windowKey: 'custom', since: '2026-11-09T08:00:00.000Z', until: '2026-11-10T08:00:00.000Z' })
})

test('自定义窗口只给 since：until = min(since + 24h, 现在)；since 在未来时取 since + 24h', () => {
  assert.deepEqual(at('since=2026-11-10T08:00:00Z'), { windowKey: 'custom', since: '2026-11-10T08:00:00.000Z', until: '2026-11-11T08:00:00.000Z' })
  assert.deepEqual(at('since=2026-11-19T06:00:00Z'), { windowKey: 'custom', since: '2026-11-19T06:00:00.000Z', until: '2026-11-19T12:00:00.000Z' })
  assert.deepEqual(at('since=2026-11-20T00:00:00Z'), { windowKey: 'custom', since: '2026-11-20T00:00:00.000Z', until: '2026-11-21T00:00:00.000Z' })
})

test('since 晚于 until 时交换两端，两端都保留', () => {
  assert.deepEqual(at('since=2026-11-19T00:00:00Z&until=2026-11-18T00:00:00Z'), { windowKey: 'custom', since: '2026-11-18T00:00:00.000Z', until: '2026-11-19T00:00:00.000Z' })
})

test('window 非法值当作没给：显式给的 since / until 不丢，按自定义窗口处理', () => {
  assert.deepEqual(at('window=bogus&since=2026-11-18T00:00:00Z&until=2026-11-18T06:00:00Z'), { windowKey: 'custom', since: '2026-11-18T00:00:00.000Z', until: '2026-11-18T06:00:00.000Z' })
  assert.deepEqual(at('window=bogus&until=2026-11-18T06:00:00Z'), { windowKey: 'custom', since: '2026-11-17T06:00:00.000Z', until: '2026-11-18T06:00:00.000Z' })
  assert.deepEqual(at('window=bogus'), { windowKey: 'last24h', since: undefined, until: undefined })
  // 时间写错的一端当作没给
  assert.deepEqual(at('since=not-a-date&until=2026-11-18T06:00:00Z'), { windowKey: 'custom', since: '2026-11-17T06:00:00.000Z', until: '2026-11-18T06:00:00.000Z' })
})

test('诊断跳转地址', () => {
  assert.equal(accountRiskLink(7), '/account-risk?accountId=7')
})
