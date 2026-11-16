import assert from 'node:assert/strict'
import { test } from 'node:test'
import { injectionSetting } from '../src/features/prompt-injection/injection-model.ts'
test('路由注入只覆盖当前路由，显式关闭覆盖全局开启', () => {
  const defaults = { taskQuality: { enabled: true } }
  assert.deepEqual(injectionSetting(defaults, {}, 'taskQuality'), { enabled: true, source: 'global_template' })
  assert.deepEqual(injectionSetting(defaults, {taskQuality:{enabled:false}}, 'taskQuality'), { enabled: false, source: 'route' })
  assert.deepEqual(injectionSetting({}, {}, 'taskQuality'), { enabled: false, source: 'builtin_default' })
  assert.equal(injectionSetting(defaults, {}, 'taskQuality').enabled, true)
})
