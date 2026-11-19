// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  applyImportDefaults,
  buildImportPlan,
  countImportRows,
  defaultImportName,
  displayBaseUrl,
  initialImportDefaults,
  isValidBaseUrl,
  maskApiKey,
  normalizeBaseUrl,
  parseImportText,
  parseTextLine,
  validateImportDefaults,
} from '../src/features/accounts/account-import.ts'

test('密钥打码与后端 mask_account_key 一致', () => {
  assert.equal(maskApiKey('sk-1234567890abcd'), 'sk-123...abcd')
  assert.equal(maskApiKey('  short  '), '***')
  assert.equal(maskApiKey('0123456789'), '***')
})

test('地址口径：协议和主机名不区分大小写，去掉末尾斜杠和默认端口', () => {
  assert.equal(normalizeBaseUrl(' HTTPS://API.Example.com:443/v1/ '), 'https://api.example.com/v1')
  assert.equal(normalizeBaseUrl('https://api.example.com'), 'https://api.example.com')
  assert.equal(normalizeBaseUrl('not a url/'), 'not a url')
})

test('文本行：空白、| 和逗号分隔，名称可含空格，地址密钥写反也认', () => {
  assert.deepEqual(parseTextLine('https://a.com sk-1 主力 账号'), { baseUrl: 'https://a.com', apiKey: 'sk-1', name: '主力 账号' })
  assert.deepEqual(parseTextLine('https://a.com|sk-2|备用'), { baseUrl: 'https://a.com', apiKey: 'sk-2', name: '备用' })
  assert.deepEqual(parseTextLine('https://a.com, sk-3'), { baseUrl: 'https://a.com', apiKey: 'sk-3', name: undefined })
  assert.deepEqual(parseTextLine('sk-4 https://a.com'), { baseUrl: 'https://a.com', apiKey: 'sk-4', name: undefined })
  assert.ok('error' in parseTextLine('sk-only'))
  assert.ok('error' in parseTextLine('a.com sk-5'))
})

test('文本行：一行里同时有空白和逗号时先按空白认，名称里的逗号保留', () => {
  assert.deepEqual(parseTextLine('https://api.example.com sk-ant-api03-XXXX 主力，备用'), {
    baseUrl: 'https://api.example.com',
    apiKey: 'sk-ant-api03-XXXX',
    name: '主力，备用',
  })
  assert.deepEqual(parseTextLine('https://a.com sk-x 主力, 备用 | 二号'), { baseUrl: 'https://a.com', apiKey: 'sk-x', name: '主力, 备用 | 二号' })
  assert.deepEqual(parseTextLine('sk-x  https://a.com/v1   名称'), { baseUrl: 'https://a.com/v1', apiKey: 'sk-x', name: '名称' })
})

test('文本行：空白认不出地址和密钥时再试 | 和逗号，名称里可以继续带分隔符', () => {
  assert.deepEqual(parseTextLine('https://a.com,sk-x,名称'), { baseUrl: 'https://a.com', apiKey: 'sk-x', name: '名称' })
  assert.deepEqual(parseTextLine('https://a.com | sk-x | 名称'), { baseUrl: 'https://a.com', apiKey: 'sk-x', name: '名称' })
  assert.deepEqual(parseTextLine('https://a.com|sk-x|主力,备用'), { baseUrl: 'https://a.com', apiKey: 'sk-x', name: '主力,备用' })
  assert.deepEqual(parseTextLine('https://a.com，sk-x，名称，带逗号'), { baseUrl: 'https://a.com', apiKey: 'sk-x', name: '名称，带逗号' })
  assert.deepEqual(parseTextLine('sk-x,https://a.com'), { baseUrl: 'https://a.com', apiKey: 'sk-x', name: undefined })
  assert.deepEqual(parseTextLine('https://a.com|sk-x|名称 带空格'), { baseUrl: 'https://a.com', apiKey: 'sk-x', name: '名称 带空格' })
})

test('文本行：地址段必须能被 URL 解析、协议 http/https、不含空白，否则整行格式错误', () => {
  assert.equal(isValidBaseUrl('https://a.com/v1'), true)
  assert.equal(isValidBaseUrl('http://127.0.0.1:8080'), true)
  assert.equal(isValidBaseUrl('ftp://a.com'), false)
  assert.equal(isValidBaseUrl('https://'), false)
  assert.equal(isValidBaseUrl('https://a.com sk-x'), false)
  assert.equal(isValidBaseUrl('https://exa mple.com'), false)
  assert.ok('error' in parseTextLine('ftp://a.com sk-x'))
  assert.ok('error' in parseTextLine('https://[bad sk-x'))
  assert.ok('error' in parseTextLine('https://a.com|sk x|名称'))
  assert.ok('error' in parseTextLine('https://a.com|'))
  assert.ok('error' in parseTextLine('https://a.com https://b.com'))
  assert.ok('error' in parseTextLine('https://a.com'))
})

test('预览表的服务地址列任何情况下都不显示密钥明文', () => {
  const secret = 'sk-ant-api03-SECRETSECRET'
  const lines = [
    `https://api.example.com ${secret} 主力，备用`,
    `https://a.com,${secret},名称`,
    `https://a.com | ${secret} | 名称`,
    `https://a.com, ${secret} 名称`,
    `ftp://a.com ${secret}`,
    `https://bad host ${secret}`,
    `${secret}`,
    `https://a.com|${secret} x|名称`,
  ]
  const rows = buildImportPlan([{ origin: '粘贴内容', entries: parseImportText(lines.join('\n')).entries }], initialImportDefaults(), [])
  assert.equal(rows.length, lines.length)
  for (const row of rows) {
    assert.ok(!row.baseUrl.includes('SECRET'), `第 ${row.position} 行地址列泄露了密钥：${row.baseUrl}`)
    assert.ok(!(row.reason ?? '').includes('SECRET'))
  }
  assert.deepEqual(rows.slice(0, 3).map((row) => row.baseUrl), ['https://api.example.com', 'https://a.com', 'https://a.com'])
  // JSON 里地址字段混进密钥也按格式错误处理，不回显
  const json = buildImportPlan([{ origin: 'a.json', entries: parseImportText(JSON.stringify([{ baseUrl: `https://a.com ${secret}`, apiKey: secret }])).entries }], initialImportDefaults(), [])
  assert.equal(json[0].state, 'invalid')
  assert.equal(json[0].baseUrl, '-')
  // 地址自带的用户名密码在预览里打码
  assert.equal(displayBaseUrl(`https://user:${secret}@a.com/v1`), 'https://***:***@a.com/v1')
  assert.equal(displayBaseUrl('https://a.com'), 'https://a.com')
  assert.equal(displayBaseUrl(`https://a.com ${secret}`), '-')
})

test('文本格式：跳过空行和注释，行号对应原文', () => {
  const result = parseImportText('# 注释\nhttps://a.com sk-aaaaaaaaaaaa\n\nbad-line\nhttps://b.com|sk-bbbbbbbbbbbb|B')
  assert.equal(result.format, 'text')
  assert.deepEqual(result.entries.map((entry) => entry.position), [2, 4, 5])
  assert.equal(result.entries[1].error !== undefined, true)
  assert.equal(result.entries[2].account?.name, 'B')
})

test('JSON 数组：字段与创建接口一致，接受 snake_case 和常见别名，未知字段与 enabled 丢弃', () => {
  const result = parseImportText(JSON.stringify([
    { name: 'n1', baseUrl: 'https://a.com', apiKey: 'k1', priority: '5', enabled: true, revision: 3, tags: 'x, y' },
    { base_url: 'https://b.com', api_key: 'k2', max_concurrent_requests: 3, auth_type: 'x-api-key' },
    { url: 'https://c.com', key: 'k3', apiKey: 'k3-canonical' },
  ]))
  assert.equal(result.format, 'json')
  assert.deepEqual(result.entries[0].account, { baseUrl: 'https://a.com', apiKey: 'k1', name: 'n1', priority: 5, tags: ['x', 'y'] })
  assert.deepEqual(result.entries[1].account, { baseUrl: 'https://b.com', apiKey: 'k2', maxConcurrentRequests: 3, authType: 'x_api_key' })
  assert.equal(result.entries[2].account?.apiKey, 'k3-canonical')
})

test('配置迁移包：读 accounts 数组；脱敏密钥标为错误，脱敏代理丢弃并提示', () => {
  const result = parseImportText(JSON.stringify({
    formatVersion: 1,
    accounts: [
      { id: 1, name: 'a', baseUrl: 'https://a.com', apiKey: '<redacted>' },
      { id: 2, name: 'b', baseUrl: 'https://b.com', apiKey: 'kb', proxyUrl: '<redacted>', overrides: { requestTimeoutSecs: 9, nope: 1, streamIdleTimeoutSecs: null } },
    ],
  }))
  assert.equal(result.format, 'config_package')
  assert.match(result.entries[0].error!, /脱敏/)
  assert.equal(result.entries[1].account?.proxyUrl, undefined)
  assert.deepEqual(result.entries[1].account?.overrides, { requestTimeoutSecs: 9 })
  assert.equal(result.entries[1].warnings.length, 1)
})

test('JSONL 每行一个对象；JSON 语法错误给出整体错误；字段校验', () => {
  const jsonl = parseImportText('{"baseUrl":"https://a.com","apiKey":"k1"}\n{"baseUrl":"https://b.com","apiKey":"k2"}\n{oops}')
  assert.equal(jsonl.format, 'jsonl')
  assert.equal(jsonl.entries.length, 3)
  assert.equal(jsonl.entries[2].error, '这一行不是合法 JSON')
  assert.match(parseImportText('[{"a":1}').error!, /JSON 解析失败/)
  const invalid = parseImportText(JSON.stringify([
    { baseUrl: 'ftp://a', apiKey: 'k' },
    { baseUrl: 'https://a.com' },
    { baseUrl: 'https://a.com', apiKey: 'k', maxConcurrentRequests: 0 },
    { baseUrl: 'https://a.com', apiKey: 'k', authType: 'basic' },
    { baseUrl: 'https://a.com', apiKey: 'k', costMultiplier: -1 },
  ]))
  assert.deepEqual(invalid.entries.map((entry) => Boolean(entry.error)), [true, true, true, true, true])
  assert.equal(parseImportText('   ').format, 'empty')
})

test('默认参数只补缺失字段；导入后启用只看开关；名称缺省用主机名加密钥后 4 位', () => {
  const defaults = { ...initialImportDefaults(), priority: '20', maxConcurrentRequests: '5', tags: ['batch'], supportedModels: ['m1'], authType: 'x_api_key' as const }
  const request = applyImportDefaults({ baseUrl: 'https://a.com', apiKey: 'sk-abcdef', priority: 3, tags: [] }, defaults)
  assert.deepEqual(request, {
    baseUrl: 'https://a.com',
    apiKey: 'sk-abcdef',
    priority: 3,
    tags: ['batch'],
    name: 'a.com-cdef',
    enabled: false,
    authType: 'x_api_key',
    usageProjectionMode: 'current_path_policy',
    maxConcurrentRequests: 5,
    supportedModels: ['m1'],
  })
  assert.equal(applyImportDefaults({ baseUrl: 'https://a.com', apiKey: 'k' }, { ...initialImportDefaults(), enabled: true }).enabled, true)
  assert.equal(defaultImportName('https://h.example.com/v1', 'abc'), 'h.example.com')
  assert.match(validateImportDefaults({ ...initialImportDefaults(), maxConcurrentRequests: '0' })!, /1 到 10000/)
  assert.match(validateImportDefaults({ ...initialImportDefaults(), costMultiplier: '0' })!, /大于 0/)
  assert.equal(validateImportDefaults(initialImportDefaults()), null)
})

test('导入计划：同批按 baseUrl+apiKey 去重，已有账号按地址+脱敏密钥标出，格式错误单列', () => {
  const pasted = parseImportText([
    'https://a.com sk-aaaaaaaaaaaa1111 一号',
    'HTTPS://A.com/ sk-aaaaaaaaaaaa1111 一号重复',
    'https://b.com sk-bbbbbbbbbbbb2222',
    'https://c.com short',
    'garbage',
  ].join('\n'))
  const file = parseImportText(JSON.stringify([{ baseUrl: 'https://a.com', apiKey: 'sk-aaaaaaaaaaaa1111' }]))
  const rows = buildImportPlan(
    [{ origin: '粘贴内容', entries: pasted.entries }, { origin: 'a.json', entries: file.entries }],
    initialImportDefaults(),
    [
      { id: 9, name: '老 B', baseUrl: 'https://b.com/', maskedApiKey: 'sk-bbb...2222' },
      { id: 10, name: '短密钥', baseUrl: 'https://c.com', maskedApiKey: '***' },
    ],
  )
  assert.deepEqual(rows.map((row) => row.state), ['new', 'duplicate', 'exists', 'new', 'invalid', 'duplicate'])
  assert.match(rows[1].reason!, /粘贴内容 第 1 条/)
  assert.match(rows[2].reason!, /#9「老 B」/)
  assert.equal(rows[5].reason, '与 粘贴内容 第 1 条重复')
  assert.equal(rows[0].maskedKey, 'sk-aaa...1111')
  assert.equal(rows[0].request?.apiKey, 'sk-aaaaaaaaaaaa1111')
  assert.deepEqual(countImportRows(rows), { new: 2, duplicate: 2, exists: 1, invalid: 1 })
})
