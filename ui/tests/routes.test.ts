import assert from 'node:assert/strict'
import { test } from 'node:test'
import { normalizeRoutePrefix, displayRoutePrefix, sortRoutes, routeEditorPath, ruleSummary } from '../src/features/routes/route-utils.ts'
test('前缀归一化、根入口与显示排序', () => {
  for (const prefix of ['', '/', ' /v1/// ']) assert.equal(normalizeRoutePrefix(prefix), '')
  assert.equal(normalizeRoutePrefix(' team/// '), '/team')
  assert.equal(displayRoutePrefix(''), '/')
  assert.equal(routeEditorPath(''), '/routes/_root')
  assert.deepEqual(sortRoutes(['/z','/ha','','/a','/na','/cc'].map(prefix=>({prefix}))).map(r=>r.prefix), ['', '/cc', '/ha', '/na', '/a', '/z'])
})
test('账号规则三态的空列表有明确文案', () => {
  assert.equal(ruleSummary('allow_all', []), '全部')
  assert.equal(ruleSummary('allow_list', []), '不允许任何项')
  assert.equal(ruleSummary('deny_list', []), '全部')
})
