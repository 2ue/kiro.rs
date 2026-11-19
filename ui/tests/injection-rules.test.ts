// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  checkInjectionTexts,
  describePrefixChanges,
  effectiveInjectionText,
  injectionCharCount,
  injectionDefaultsAlready,
  injectionPrefixChanges,
  injectionTextError,
  isInjectionTextEdited,
  routeInjectionMatrix,
  withAllInjectionDefaults,
  type InjectionRouteLike,
} from '../src/features/prompt-injection/injection-rules.ts'
import type { InjectionCatalogEntry, InjectionId, PromptInjectionConfig } from '../src/types/runtime.ts'

function entry(id: InjectionId, name: string, defaultText: string, patch: Partial<InjectionCatalogEntry> = {}): InjectionCatalogEntry {
  return {
    id,
    code: id,
    name,
    category: 'operator',
    position: 'system',
    trigger: '开关生效即注入',
    dependsOn: null,
    impactWhenOff: '',
    recommendation: 'user_choice',
    recommendationNote: '',
    defaultEnabled: false,
    strictBlocks: id !== 'identityPrompt',
    editable: true,
    texts: [{ key: id, label: id, defaultText }],
    readOnlyVariants: [],
    ...patch,
  }
}

const catalog: InjectionCatalogEntry[] = [
  entry('taskQuality', '任务质量', '<task>规则</task>', { recommendation: 'keep_on' }),
  entry('languageConstraint', '语言约束', '<language/>'),
  entry('custom', '自定义', ''),
  entry('identityPrompt', '身份声明', 'You are Claude.'),
]

const config = (patch: Partial<PromptInjectionConfig> = {}): PromptInjectionConfig => ({
  applyToCountTokens: true,
  defaults: {
    taskQuality: { enabled: true },
    languageConstraint: { enabled: false },
    custom: { enabled: true },
    identityPrompt: { enabled: false },
  },
  texts: {},
  ...patch,
})

const route = (id: number, prefix: string, promptInjection: InjectionRouteLike['promptInjection'] = {}, enabled = true): InjectionRouteLike => ({
  id,
  prefix,
  name: prefix || '默认',
  enabled,
  promptInjection,
})

test('后端口径的生效文本与“已修改”：去掉首尾空白，空白或与内置相同都不算修改', () => {
  const variant = catalog[0]!.texts[0]!
  assert.equal(effectiveInjectionText({}, variant), '<task>规则</task>')
  assert.equal(effectiveInjectionText({ taskQuality: '   ' }, variant), '<task>规则</task>')
  assert.equal(isInjectionTextEdited({ taskQuality: '  ' }, variant), false)
  assert.equal(isInjectionTextEdited({ taskQuality: ' <task>规则</task>\n' }, variant), false)
  assert.equal(isInjectionTextEdited({ taskQuality: '新规则' }, variant), true)
  assert.equal(effectiveInjectionText({ taskQuality: '  新规则 ' }, variant), '新规则')
  const custom = catalog[2]!.texts[0]!
  assert.equal(isInjectionTextEdited({}, custom), false)
  assert.equal(isInjectionTextEdited({ custom: '自定义内容' }, custom), true)
})

test('字符数按码点计，与后端 chars().count() 一致', () => {
  assert.equal(injectionCharCount('中文'), 2)
  assert.equal(injectionCharCount('😀a'), 2)
})

test('长度校验：单段上限与合计上限', () => {
  const limits = { maxTextChars: 20, maxTotalTextChars: 41 }
  const fine = checkInjectionTexts({}, catalog, limits)
  assert.equal(fine.total, injectionCharCount('<task>规则</task>') + injectionCharCount('<language/>') + injectionCharCount('You are Claude.'))
  assert.equal(fine.total, 41)
  assert.equal(fine.overTotal, false)
  assert.deepEqual(fine.overItems, [])
  assert.equal(checkInjectionTexts({ custom: 'x' }, catalog, limits).overTotal, true)
  assert.equal(injectionTextError({ taskQuality: 'short' }, catalog, { maxTextChars: 100, maxTotalTextChars: 100 }), null)
  assert.match(injectionTextError({ custom: 'x'.repeat(16) }, catalog, { maxTextChars: 15, maxTotalTextChars: 1000 }) ?? '', /自定义的文本 16 字符，超过单段上限 15/)
  assert.match(injectionTextError({ custom: 'x'.repeat(10) }, catalog, { maxTextChars: 100, maxTotalTextChars: 40 }) ?? '', /合计 \d+ 字符，超过上限 40/)
})

test('全部默认值设为关 / 全部恢复新装默认只改全局默认', () => {
  const fresh = withAllInjectionDefaults(config(), catalog, 'fresh')
  assert.deepEqual(Object.values(fresh.defaults).map((item) => item.enabled), [false, false, false, false])
  const withNewDefault = [{ ...catalog[0]!, defaultEnabled: true }, ...catalog.slice(1)]
  assert.equal(withAllInjectionDefaults(config(), withNewDefault, 'fresh').defaults.taskQuality.enabled, true)
  assert.equal(withAllInjectionDefaults(config(), withNewDefault, 'off').defaults.taskQuality.enabled, false)
  assert.equal(injectionDefaultsAlready(config(), catalog, 'off'), false)
  assert.equal(injectionDefaultsAlready(fresh, catalog, 'off'), true)
  assert.equal(injectionDefaultsAlready(fresh, withNewDefault, 'fresh'), false)
  // 文本与 count_tokens 开关不受影响。
  const edited = config({ texts: { custom: 'x' }, applyToCountTokens: false })
  const off = withAllInjectionDefaults(edited, catalog, 'off')
  assert.deepEqual(off.texts, { custom: 'x' })
  assert.equal(off.applyToCountTokens, false)
})

test('路由矩阵：显式设置、跟随默认、strict 拦截与空文本都标出来，顺序跟传入一致', () => {
  const rows = routeInjectionMatrix(
    [route(1, ''), route(2, '/cc', { taskQuality: { enabled: false }, identityPrompt: { enabled: true } }, false)],
    catalog,
    config(),
    false,
  )
  assert.deepEqual(rows.map((row) => [row.id, row.prefix, row.enabled]), [[1, '/', true], [2, '/cc', false]])
  const cells = (index: number) => rows[index]!.cells.map((cell) => `${cell.id}:${cell.state}${cell.explicit ? '·' : ''}`)
  assert.deepEqual(cells(0), ['taskQuality:on', 'languageConstraint:off', 'custom:blocked', 'identityPrompt:off'])
  assert.deepEqual(cells(1), ['taskQuality:off·', 'languageConstraint:off', 'custom:blocked', 'identityPrompt:on·'])
  assert.equal(rows[0]!.cells[2]!.reason, '文本为空，开启也不注入')

  const strict = routeInjectionMatrix([route(1, '', { identityPrompt: { enabled: true } })], catalog, config({ texts: { custom: '内容' } }), true)
  assert.deepEqual(strict[0]!.cells.map((cell) => cell.state), ['blocked', 'off', 'blocked', 'on'])
  assert.equal(strict[0]!.cells[0]!.reason, 'anthropic-strict 模式下不注入')
})

test('缓存前缀变化：文本改动只影响开着这一项的路由', () => {
  const routes = [route(1, ''), route(2, '/cc', { taskQuality: { enabled: false } })]
  const changes = injectionPrefixChanges(
    catalog,
    { config: config(), routes },
    { config: config({ texts: { taskQuality: '新规则' } }), routes },
    false,
  )
  assert.deepEqual(changes, [{ id: 'taskQuality', name: '任务质量', routes: ['/'] }])
  assert.equal(describePrefixChanges(changes), '任务质量（/）')
})

test('缓存前缀变化：默认开关只影响跟随默认的路由，显式设置的路由不受影响', () => {
  const routes = [route(1, ''), route(2, '/cc', { identityPrompt: { enabled: false } }), route(3, '/ha')]
  const next = config({ defaults: { ...config().defaults, identityPrompt: { enabled: true } } })
  const changes = injectionPrefixChanges(catalog, { config: config(), routes }, { config: next, routes }, false)
  assert.deepEqual(changes, [{ id: 'identityPrompt', name: '身份声明', routes: ['/', '/ha'] }])
})

test('缓存前缀变化：不会注入的改动、空自定义文本、新路由都不算', () => {
  const routes = [route(1, '/cc')]
  // 语言约束关着，改它的文本不改变请求内容。
  assert.deepEqual(injectionPrefixChanges(catalog, { config: config(), routes }, { config: config({ texts: { languageConstraint: 'x' } }), routes }, false), [])
  // strict 模式下任务质量不注入，改文本不影响。
  assert.deepEqual(injectionPrefixChanges(catalog, { config: config(), routes }, { config: config({ texts: { taskQuality: 'x' } }), routes }, true), [])
  // 只改空白，生效文本不变。
  assert.deepEqual(injectionPrefixChanges(catalog, { config: config(), routes }, { config: config({ texts: { taskQuality: '  ' } }), routes }, false), [])
  // 自定义开着但文本从空变成有内容：会开始注入。
  assert.deepEqual(
    injectionPrefixChanges(catalog, { config: config(), routes }, { config: config({ texts: { custom: '内容' } }), routes }, false).map((change) => change.id),
    ['custom'],
  )
  // 新建的路由没有旧缓存。
  assert.deepEqual(injectionPrefixChanges(catalog, { config: config(), routes: [] }, { config: config({ texts: { taskQuality: 'x' } }), routes }, false), [])
})

test('缓存前缀变化：单条路由的开关改动', () => {
  const before = [route(5, '/dfcache/a')]
  const after = [route(5, '/dfcache/a', { taskQuality: { enabled: false }, languageConstraint: { enabled: false } })]
  const changes = injectionPrefixChanges(catalog, { config: config(), routes: before }, { config: config(), routes: after }, false)
  // 语言约束本来就关，显式写关不改变内容。
  assert.deepEqual(changes, [{ id: 'taskQuality', name: '任务质量', routes: ['/dfcache/a'] }])
})
