// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { accountExportFilename, exportHasRedactedSecrets, pickExportAccounts } from '../src/features/accounts/account-export.ts'
import { buildImportPlan, initialImportDefaults, parseImportText } from '../src/features/accounts/account-import.ts'

const pkgAccounts = [
  { id: 3, name: 'c', baseUrl: 'https://c.com', apiKey: 'kc-0123456789', enabled: true, priority: 1, skipNonStreamUsageProjection: false, overrides: {}, notes: null },
  { id: 1, name: 'a', baseUrl: 'https://a.com', apiKey: 'ka-0123456789', enabled: false, tags: ['x'] },
  { id: 2, name: 'b', baseUrl: 'https://b.com', apiKey: '<redacted>' },
]

test('按 ID 顺序挑出账号，只保留创建接口字段，不存在的 ID 跳过', () => {
  const picked = pickExportAccounts(pkgAccounts, [1, 3, 99])
  assert.deepEqual(picked, [
    { name: 'a', baseUrl: 'https://a.com', apiKey: 'ka-0123456789', enabled: false, tags: ['x'] },
    { name: 'c', baseUrl: 'https://c.com', apiKey: 'kc-0123456789', enabled: true, priority: 1, overrides: {} },
  ])
  assert.equal(pickExportAccounts(pkgAccounts, null).length, 3)
})

test('不带密钥导出能识别出来；带密钥导出的文件能被批量导入原样识别', () => {
  assert.equal(exportHasRedactedSecrets(pickExportAccounts(pkgAccounts, [2])), true)
  const exported = pickExportAccounts(pkgAccounts, [3, 1])
  assert.equal(exportHasRedactedSecrets(exported), false)
  const parsed = parseImportText(JSON.stringify(exported, null, 2))
  const rows = buildImportPlan([{ origin: 'export.json', entries: parsed.entries }], initialImportDefaults(), [])
  assert.deepEqual(rows.map((row) => [row.state, row.name, row.request?.priority]), [['new', 'c', 1], ['new', 'a', undefined]])
  // 文件里的 enabled 不生效，导入后启用只看开关（默认关）
  assert.deepEqual(rows.map((row) => row.request?.enabled), [false, false])
})

test('文件名带时间戳', () => {
  assert.equal(accountExportFilename(new Date(2026, 10, 19, 8, 5, 9)), 'claude-pool-accounts-20261119-080509.json')
})

test('原始用量口径随账号导出导入保留，旧格式默认仍沿用旧模式', () => {
  const exported = pickExportAccounts([{ ...pkgAccounts[0], rawUsageSource: 'upstream_last' }], null)
  const parsed = parseImportText(JSON.stringify(exported))
  const rows = buildImportPlan([{ origin: 'export.json', entries: parsed.entries }], initialImportDefaults(), [])
  assert.equal(rows[0].request?.rawUsageSource, 'upstream_last')
})
