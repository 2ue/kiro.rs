import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const compiledRoot = mkdtempSync(join(tmpdir(), 'kiro-cost-format-'))

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
      join(repoRoot, 'ui/src/lib/format.ts'),
      join(repoRoot, 'admin-ui/src/lib/format.ts'),
    ],
    { stdio: 'inherit' },
  )

  const ui = await import(pathToFileURL(join(compiledRoot, 'ui/src/lib/format.js')))
  const admin = await import(pathToFileURL(join(compiledRoot, 'admin-ui/src/lib/format.js')))

  const formatters = [
    ['ui', ui.formatUsd, ui.formatUsdDetailed, ui.formatUsdCsv],
    ['admin-ui', admin.formatUsd, admin.formatUsdDetailed, admin.formatUsdCsv],
  ]

  const detailedCases = [
    [0, '$0.00000000', '0.00000000'],
    [0.0000000049, '$0.00000000', '0.00000000'],
    [0.000000005, '$0.00000001', '0.00000001'],
    [0.00000123, '$0.00000123', '0.00000123'],
    [0.99999999, '$0.99999999', '0.99999999'],
    [1, '$1.00000000', '1.00000000'],
    [-2, '-$2.00000000', '-2.00000000'],
  ]

  for (const [name, summary, detailed, csv] of formatters) {
    assert.equal(summary(0.5), '$0.500000', `${name} small summary precision`)
    assert.equal(summary(2), '$2.00', `${name} positive summary precision`)
    assert.equal(summary(-2), '-$2.00', `${name} negative summary precision`)
    assert.equal(summary(Number.NaN), '-', `${name} NaN summary`)
    assert.equal(summary(null), '-', `${name} null summary`)
    assert.equal(detailed(Number.NaN), '-', `${name} NaN detail`)
    assert.equal(csv(Number.NaN), '', `${name} NaN CSV`)
    assert.equal(csv(null), '', `${name} null CSV`)

    for (const [value, expectedDetailed, expectedCsv] of detailedCases) {
      assert.equal(detailed(value), expectedDetailed, `${name} detail ${value}`)
      assert.equal(csv(value), expectedCsv, `${name} CSV ${value}`)
    }
  }

  console.log('cost format contract: PASS (ui + admin-ui)')
} finally {
  rmSync(compiledRoot, { recursive: true, force: true })
}
