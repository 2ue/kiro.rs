// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { MAX_TAGS, MAX_TAG_LENGTH, addTags, cleanTags, normalizeTags, removeTags, splitTagInput, tagsDroppedByLimit } from '../src/components/patterns/tag-utils.ts'
import { buildAccountRequest, defaultAccountForm } from '../src/features/accounts/account-utils.ts'

test('标签去空白、去重（区分大小写）、保持顺序', () => {
  assert.deepEqual(normalizeTags([' a ', 'b', 'a', '', '  ', 'A']), ['a', 'b', 'A'])
})

test('一组全新的标签（导入等）：单个截断到 64 字符，最多保留 32 个', () => {
  const long = 'x'.repeat(MAX_TAG_LENGTH + 10)
  assert.equal(normalizeTags([long])[0].length, MAX_TAG_LENGTH)
  const many = Array.from({ length: MAX_TAGS + 5 }, (_, i) => `t${i}`)
  assert.equal(normalizeTags(many).length, MAX_TAGS)
})

test('输入框里的逗号（中英文）、分号、换行都拆成多个标签', () => {
  assert.deepEqual(splitTagInput('a, b，c;d；e\nf'), ['a', 'b', 'c', 'd', 'e', 'f'])
  assert.deepEqual(splitTagInput(' , ,'), [])
})

test('追加时已有标签不重复，移除只去掉精确匹配的', () => {
  assert.deepEqual(addTags(['a', 'b'], ['b', 'c']), ['a', 'b', 'c'])
  assert.deepEqual(removeTags(['a', 'b', 'c'], ['b', 'B', ' c ']), ['a'])
})

test('已有标签只去空白去重，不截断长度、不限数量', () => {
  const long = 'x'.repeat(MAX_TAG_LENGTH + 10)
  const many = Array.from({ length: MAX_TAGS + 5 }, (_, i) => `t${i}`)
  assert.deepEqual(cleanTags([` ${long} `, long, ...many, ' ', 't0']), [long, ...many])
})

test('上限只作用于新加入的标签：已有超过 32 个时不再追加但不删已有的；新输入的标签按 64 字符截断', () => {
  const many = Array.from({ length: MAX_TAGS + 3 }, (_, i) => `t${i}`)
  assert.deepEqual(addTags(many, ['new']), many)
  assert.deepEqual(tagsDroppedByLimit(many, ['new', 't1', ' ']), ['new'])
  const almost = many.slice(0, MAX_TAGS - 1)
  assert.deepEqual(addTags(almost, ['n1', 'n2']), [...almost, 'n1'])
  assert.deepEqual(tagsDroppedByLimit(almost, ['n1', 'n2']), ['n2'])
  const long = 'y'.repeat(MAX_TAG_LENGTH + 10)
  assert.deepEqual(addTags([long], splitTagInput('z'.repeat(MAX_TAG_LENGTH + 1))), [long, 'z'.repeat(MAX_TAG_LENGTH)])
  // 从已有标签候选里点选的是池里现成的值，原样加入；移除也按原值精确匹配
  assert.deepEqual(addTags([], [long]), [long])
  assert.deepEqual(removeTags([long, 'a'], [long]), ['a'])
})

test('保存账号时已有标签只去空白去重，不截断数量和长度', () => {
  const long = 'k'.repeat(MAX_TAG_LENGTH + 20)
  const many = Array.from({ length: MAX_TAGS + 4 }, (_, i) => `tag${i}`)
  const request = buildAccountRequest({ ...defaultAccountForm(), name: 'a', baseUrl: 'https://a.com', apiKey: 'k', tags: [long, ...many, ` ${long}`, ''] })
  assert.deepEqual(request.tags, [long, ...many])
})
