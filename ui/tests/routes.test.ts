import assert from 'node:assert/strict'
import { test } from 'node:test'
import { normalizeRoutePrefix, displayRoutePrefix, sortRoutes, routeEditorPath, ruleSummary } from '../src/features/routes/route-utils.ts'
test('前缀归一化、根入口与显示排序', () => {
  for (const prefix of ['', '/', ' /v1/// ']) assert.equal(normalizeRoutePrefix(prefix), '')
  assert.equal(normalizeRoutePrefix(' team/// '), '/team')
  assert.equal(displayRoutePrefix(''), '/')
  assert.equal(routeEditorPath(3), '/routes/3')
  assert.deepEqual(sortRoutes(['/z','/ha','','/a','/na','/cc'].map(prefix=>({prefix}))).map(r=>r.prefix), ['', '/cc', '/ha', '/na', '/a', '/z'])
})
test('账号规则三态的空列表有明确文案', () => {
  assert.equal(ruleSummary('allow_all', []), '全部')
  assert.equal(ruleSummary('allow_list', []), '不允许任何项')
  assert.equal(ruleSummary('deny_list', []), '全部')
})
import {
  CUSTOM_ROUTE_PREFIX,
  customRouteName,
  isBuiltinRoute,
  normalizeCustomRoutePrefix,
  routeBaseUrl,
  routeClaudeCodeEnv,
} from '../src/features/routes/route-utils.ts'

test('自定义路由只接受 /dfcache/{name}，任意根级前缀被拒绝', () => {
  assert.equal(CUSTOM_ROUTE_PREFIX, '/dfcache/')
  assert.equal(normalizeCustomRoutePrefix('team-a'), '/dfcache/team-a')
  assert.equal(normalizeCustomRoutePrefix(' /dfcache/Team.B_1/ '), '/dfcache/team.b_1')
  for (const bad of ['', '/', 'a/b', 'has space', 'x'.repeat(65), '/dfcache/', 'dfcache']) {
    assert.equal(normalizeCustomRoutePrefix(bad), null, JSON.stringify(bad))
  }
  // 任何名字都只会落在 /dfcache/ 下，所以 /api、/ui 这类输入不会变成根级入口。
  assert.equal(normalizeCustomRoutePrefix('/api'), '/dfcache/api')
  assert.equal(normalizeCustomRoutePrefix('/ui'), '/dfcache/ui')
  assert.equal(customRouteName('/dfcache/team'), 'team')
  assert.equal(customRouteName('/cc'), null)
  for (const builtin of ['', '/v1', '/cc', '/ha', '/na']) assert.equal(isBuiltinRoute(builtin), true)
  assert.equal(isBuiltinRoute('/dfcache/team'), false)
})

test('接入地址：根入口 base 是服务根，其他入口 base 是前缀本身', () => {
  const origin = 'https://proxy.example.com/'
  assert.equal(routeBaseUrl('', origin), 'https://proxy.example.com')
  assert.equal(routeBaseUrl('/cc', origin), 'https://proxy.example.com/cc')
  assert.equal(routeBaseUrl('/dfcache/team', origin), 'https://proxy.example.com/dfcache/team')
  assert.match(routeClaudeCodeEnv('/cc', origin), /^ANTHROPIC_BASE_URL=https:\/\/proxy\.example\.com\/cc\n/)
})
