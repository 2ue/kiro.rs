// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { extractErrorMessage } from '../src/lib/utils.ts'

test('后端返回 JSON 错误时取 error.message', () => {
  const error = { message: 'Request failed with status code 400', response: { status: 400, data: { error: { type: 'invalid_request', message: '路由前缀已存在' } } } }
  assert.equal(extractErrorMessage(error), '路由前缀已存在')
})

test('请求体反序列化失败的纯文本响应要显示出来，而不是英文状态码', () => {
  const text = 'Failed to deserialize the JSON body into the target type: features.payloadGuard: unknown field `foo`'
  const error = { message: 'Request failed with status code 422', response: { status: 422, data: text } }
  assert.equal(extractErrorMessage(error), `请求参数无效（422）：${text}`)
  const long = { message: 'x', response: { status: 422, data: 'a'.repeat(400) } }
  assert.ok(extractErrorMessage(long).endsWith('…'))
})

test('HTML 错误页和空响应体回退到原始错误信息', () => {
  assert.equal(extractErrorMessage({ message: 'Network Error', response: { status: 502, data: '<html>bad gateway</html>' } }), 'Network Error')
  assert.equal(extractErrorMessage({ message: 'timeout of 1000ms exceeded' }), 'timeout of 1000ms exceeded')
})
