import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import test from 'node:test'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..')

test('the UI canonicalizes official legacy Claude 3.5 IDs for external pools', async () => {
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
      ],
      { stdio: 'inherit' },
    )

    const { toExternalPoolTestModelName } = await import(
      pathToFileURL(join(compiledRoot, 'ui/src/lib/test-models.js'))
    )

    // External pools receive standard Claude Code ids: dotted spellings become dashed, dates are
    // kept, -thinking/[1m] variants collapse to the base id, and non-Claude ids are rejected.
    assert.equal(toExternalPoolTestModelName('claude-3-5-sonnet-20241022'), 'claude-3-5-sonnet-20241022')
    assert.equal(toExternalPoolTestModelName('claude-3.5-sonnet'), 'claude-3-5-sonnet')
    assert.equal(
      toExternalPoolTestModelName('claude-3-5-haiku-20241022-thinking[1m]'),
      'claude-3-5-haiku-20241022',
    )
    assert.equal(toExternalPoolTestModelName('claude-3-5-opus-20241022'), null)
    assert.equal(toExternalPoolTestModelName('tenant-claude-3-5-sonnet'), null)
  } finally {
    rmSync(compiledRoot, { recursive: true, force: true })
  }
})
