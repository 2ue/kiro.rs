import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import test from 'node:test'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..')

test('both UI surfaces canonicalize official legacy Claude 3.5 IDs for external pools', async () => {
  const compiledRoot = mkdtempSync(join(tmpdir(), 'kiro-claude-model-contract-'))
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
        join(repoRoot, 'ui/src/lib/test-models.ts'),
        join(repoRoot, 'admin-ui/src/lib/test-models.ts'),
      ],
      { stdio: 'inherit' },
    )

    const surfaces = await Promise.all([
      import(pathToFileURL(join(compiledRoot, 'ui/src/lib/test-models.js'))),
      import(pathToFileURL(join(compiledRoot, 'admin-ui/src/lib/test-models.js'))),
    ])

    for (const { toClaudeCodeModelName } of surfaces) {
      assert.equal(toClaudeCodeModelName('claude-3-5-sonnet-20241022'), 'sonnet-3.5')
      assert.equal(toClaudeCodeModelName('claude-3.5-sonnet'), 'sonnet-3.5')
      assert.equal(
        toClaudeCodeModelName('claude-3-5-haiku-20241022-thinking[1m]'),
        'haiku-3.5-thinking[1m]',
      )
      assert.equal(toClaudeCodeModelName('claude-3-5-opus-20241022'), 'claude-3-5-opus-20241022')
      assert.equal(toClaudeCodeModelName('tenant-claude-3-5-sonnet'), 'tenant-claude-3-5-sonnet')
    }
  } finally {
    rmSync(compiledRoot, { recursive: true, force: true })
  }
})
