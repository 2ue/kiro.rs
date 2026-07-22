import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const compiledRoot = mkdtempSync(join(tmpdir(), 'kiro-request-api-key-id-'))

try {
  execFileSync(
    join(repoRoot, 'ui/node_modules/.bin/tsc'),
    [
      '--pretty', 'false',
      '--target', 'ES2022',
      '--module', 'ES2022',
      '--moduleResolution', 'Bundler',
      '--skipLibCheck', 'true',
      '--rootDir', repoRoot,
      '--outDir', compiledRoot,
      join(repoRoot, 'ui/src/lib/request-api-key-id.ts'),
      join(repoRoot, 'admin-ui/src/lib/request-api-key-id.ts'),
    ],
    { stdio: 'inherit' },
  )

  const modules = [
    ['ui', await import(pathToFileURL(join(compiledRoot, 'ui/src/lib/request-api-key-id.js')))],
    ['admin-ui', await import(pathToFileURL(join(compiledRoot, 'admin-ui/src/lib/request-api-key-id.js')))],
  ]
  const digest = '0123456789abcdef'.repeat(4)

  for (const [name, apiKeyId] of modules) {
    assert.equal(apiKeyId.normalizeRequestApiKeyId(`  ${digest.toUpperCase()}  `), digest, `${name} normalizes digest`)
    assert.equal(apiKeyId.formatRequestApiKeyId(digest), '01234567...89abcdef', `${name} short display`)
    assert.equal(apiKeyId.normalizeRequestApiKeyId('sk-must-never-be-rendered'), undefined, `${name} rejects raw key`)
    assert.equal(apiKeyId.normalizeRequestApiKeyId('a'.repeat(63)), undefined, `${name} rejects short digest`)
    assert.equal(apiKeyId.normalizeRequestApiKeyId('g'.repeat(64)), undefined, `${name} rejects non-hex digest`)
    assert.equal(apiKeyId.formatRequestApiKeyId(undefined), '-', `${name} missing display`)
  }

  const sourceContracts = [
    {
      name: 'ui',
      types: 'ui/src/types/api.ts',
      list: 'ui/src/features/usage/usage-page.tsx',
      detail: 'ui/src/features/usage/usage-detail-modal.tsx',
    },
    {
      name: 'admin-ui',
      types: 'admin-ui/src/types/api.ts',
      list: 'admin-ui/src/components/usage-records-panel.tsx',
      detail: 'admin-ui/src/components/usage-records-panel.tsx',
    },
  ]

  for (const contract of sourceContracts) {
    const types = readFileSync(join(repoRoot, contract.types), 'utf8')
    const list = readFileSync(join(repoRoot, contract.list), 'utf8')
    const detail = readFileSync(join(repoRoot, contract.detail), 'utf8')

    assert.ok((types.match(/requestApiKeyId\?: string/g) || []).length >= 2, `${contract.name} record and query types`)
    assert.match(list, /next\.requestApiKeyId\s*=\s*normalizedRequestApiKeyId/, `${contract.name} query parameter`)
    assert.match(list, /request_api_key_id/, `${contract.name} CSV attribution`)
    assert.match(list, /请求渠道 ID/, `${contract.name} explicit filter label`)
    assert.match(list, /RequestApiKeyIdDisplay/, `${contract.name} list short display`)
    assert.match(detail, /RequestApiKeyIdDisplay/, `${contract.name} detail short display`)
  }

  console.log('request API key ID contract: PASS (ui + admin-ui)')
} finally {
  rmSync(compiledRoot, { recursive: true, force: true })
}
