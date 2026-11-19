// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { proxyPasswordIsSet, proxyPasswordPatch } from '../src/features/runtime/proxy-password.ts'

test('读取到掩码或老后端的脱敏值都算已设置，null / 空串算未设置', () => {
  assert.equal(proxyPasswordIsSet('********'), true)
  assert.equal(proxyPasswordIsSet('ab...yz'), true)
  assert.equal(proxyPasswordIsSet(null), false)
  assert.equal(proxyPasswordIsSet(undefined), false)
  assert.equal(proxyPasswordIsSet(''), false)
})

test('输入框为空（没动过或输入后又删空）时不带 proxyPassword 字段，后端保持原密码', () => {
  assert.deepEqual(proxyPasswordPatch('', 'keep'), {})
  assert.equal('proxyPassword' in proxyPasswordPatch('', 'keep'), false)
})

test('输入新值时带新密码；空格也是密码的一部分，原样提交', () => {
  assert.deepEqual(proxyPasswordPatch('new-pass', 'keep'), { proxyPassword: 'new-pass' })
  assert.deepEqual(proxyPasswordPatch(' p ', 'keep'), { proxyPassword: ' p ' })
})

test('只有确认清除后才提交 null', () => {
  assert.deepEqual(proxyPasswordPatch('', 'clear'), { proxyPassword: null })
  // 清除优先于输入框内容（清除时输入框被禁用并清空，这里兜底）。
  assert.deepEqual(proxyPasswordPatch('typed', 'clear'), { proxyPassword: null })
})
