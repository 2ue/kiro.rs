// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { rankUsageFilter } from '../src/features/overview/rank-links.ts'
import { previewRoutePrefix, routePrefixForEndpoint } from '../src/features/routes/route-utils.ts'

const KEY_ID = 'A'.repeat(64)

test('入口推路由前缀与后端 usage_route_prefix_for_endpoint 同一口径', () => {
  for (const [endpoint, expected] of [
    ['/v1/messages', '/'],
    ['/v1/messages/count_tokens', '/'],
    ['/cc/v1/messages', '/cc'],
    ['/cc/v1/messages/count_tokens', '/cc'],
    ['/dfcache/team-a/v1/messages', '/dfcache/team-a'],
    // 自定义路由名本身含 v1：取最后一个 /v1/ 之前的部分。
    ['/dfcache/v1/v1/messages', '/dfcache/v1'],
    ['/cc/v1', '/cc'],
    ['  /ha/v1/messages  ', '/ha'],
    ['', 'unknown'],
    ['unknown', 'unknown'],
    ['/healthz', '/healthz'],
  ]) {
    assert.equal(routePrefixForEndpoint(endpoint), expected, endpoint)
  }
  assert.equal(routePrefixForEndpoint(undefined), 'unknown')
  assert.equal(routePrefixForEndpoint(null), 'unknown')
})

test('注入预览的默认路由写成 /v1，其余为归一化前缀', () => {
  assert.equal(previewRoutePrefix(''), '/v1')
  assert.equal(previewRoutePrefix('/v1'), '/v1')
  assert.equal(previewRoutePrefix('/'), '/v1')
  assert.equal(previewRoutePrefix('/cc/'), '/cc')
  assert.equal(previewRoutePrefix('/dfcache/team'), '/dfcache/team')
})

test('路由页签按路径所属的路由前缀筛，不再只按路径子串（会混进其它路由）', () => {
  assert.deepEqual(rankUsageFilter('paths', '/v1/messages'), { route: '/', endpoint: '/v1/messages' })
  assert.deepEqual(rankUsageFilter('paths', '/cc/v1/messages/count_tokens'), { route: '/cc', endpoint: '/cc/v1/messages/count_tokens' })
  assert.deepEqual(rankUsageFilter('paths', '/dfcache/v1/v1/messages'), { route: '/dfcache/v1', endpoint: '/dfcache/v1/v1/messages' })
  // 推不出路由的路径只按路径筛。
  assert.deepEqual(rankUsageFilter('paths', '/healthz'), { endpoint: '/healthz' })
})

test('各维度的筛选键；unknown 行与错误页签不可点', () => {
  assert.deepEqual(rankUsageFilter('models', 'claude-sonnet-4-5'), { model: 'claude-sonnet-4-5' })
  assert.deepEqual(rankUsageFilter('accounts', '12'), { accountId: 12 })
  assert.deepEqual(rankUsageFilter('keys', KEY_ID), { requestApiKeyId: KEY_ID.toLowerCase() })
  for (const dimension of ['models', 'accounts', 'keys', 'paths', 'errors'] as const) {
    assert.equal(rankUsageFilter(dimension, 'unknown'), null, dimension)
    assert.equal(rankUsageFilter(dimension, ''), null, dimension)
  }
  assert.equal(rankUsageFilter('errors', 'rate_limit_error'), null)
  assert.equal(rankUsageFilter('accounts', '0'), null)
  assert.equal(rankUsageFilter('accounts', '1.5'), null)
  // 不是完整渠道 ID 的 key 在用量页会被当成无效值丢掉，跳过去等于没筛：不可点。
  assert.equal(rankUsageFilter('keys', 'legacy-key'), null)
})
