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
    ],
    { stdio: 'inherit' },
  )

  const apiKeyId = await import(pathToFileURL(join(compiledRoot, 'ui/src/lib/request-api-key-id.js')))
  const digest = '0123456789abcdef'.repeat(4)

  assert.equal(apiKeyId.normalizeRequestApiKeyId(`  ${digest.toUpperCase()}  `), digest, 'normalizes digest')
  assert.equal(apiKeyId.formatRequestApiKeyId(digest), '01234567...89abcdef', 'short display')
  assert.equal(apiKeyId.normalizeRequestApiKeyId('sk-must-never-be-rendered'), undefined, 'rejects raw key')
  assert.equal(apiKeyId.normalizeRequestApiKeyId('a'.repeat(63)), undefined, 'rejects short digest')
  assert.equal(apiKeyId.normalizeRequestApiKeyId('g'.repeat(64)), undefined, 'rejects non-hex digest')
  assert.equal(apiKeyId.formatRequestApiKeyId(undefined), '-', 'missing display')

  const types = readFileSync(join(repoRoot, 'ui/src/types/api.ts'), 'utf8')
  const list = readFileSync(join(repoRoot, 'ui/src/features/usage/usage-page.tsx'), 'utf8')
  const detail = readFileSync(join(repoRoot, 'ui/src/features/usage/usage-detail-modal.tsx'), 'utf8')

  assert.ok((types.match(/requestApiKeyId\?: string/g) || []).length >= 2, 'record and query types')
  assert.match(list, /next\.requestApiKeyId\s*=\s*normalizedRequestApiKeyId/, 'query parameter')
  assert.match(list, /request_api_key_id/, 'CSV attribution')
  assert.match(list, /请求渠道 ID/, 'explicit filter label')
  assert.match(list, /RequestApiKeyIdDisplay/, 'list short display')
  assert.match(detail, /RequestApiKeyIdDisplay/, 'detail short display')

  console.log('request API key ID contract: PASS (ui)')
} finally {
  rmSync(compiledRoot, { recursive: true, force: true })
}
