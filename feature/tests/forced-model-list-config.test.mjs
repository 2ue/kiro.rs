import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const out = mkdtempSync(join(tmpdir(), 'kiro-forced-model-list-'))
try {
  writeFileSync(join(out, 'package.json'), '{"type":"module"}')
  writeFileSync(join(out, 'tsconfig.json'), JSON.stringify({
    compilerOptions: {
      target: 'ES2022', module: 'ES2022', moduleResolution: 'Bundler', skipLibCheck: true,
      baseUrl: join(root, 'ui'), paths: { '@/*': ['./src/*'] },
      rootDir: root, outDir: join(out, 'compiled'),
    },
    files: [join(root, 'ui/src/lib/runtime-config-defaults.ts')],
  }))
  execFileSync(join(root, 'ui/node_modules/.bin/tsc'), ['-p', join(out, 'tsconfig.json')], { stdio: 'inherit' })
  const config = await import(pathToFileURL(join(out, 'compiled/ui/src/lib/runtime-config-defaults.js')))
  const defaults = config.defaultForcedModelMapping()
  assert.equal(defaults.listedModels.length, 25)
  assert.ok(defaults.listedModels.includes('claude-haiku-5-5'))
  assert.equal(config.forcedModelMappingError(defaults), null)
  assert.deepEqual(config.normalizeForcedModelMapping({ enabled: true }).listedModels, defaults.listedModels)
  assert.deepEqual(config.normalizeForcedModelMapping({ listedModels: [] }).listedModels, [])
  const normalized = config.normalizeForcedModelMapping({
    listedModels: [' Claude-Haiku-5.5 ', 'claude-haiku-5-5', '', 'claude-opus-4-5-20251101'],
    modelOverrides: { 'Claude-Opus-4.5-20251101': 'claude-sonnet-4.5' },
  })
  assert.deepEqual(normalized.listedModels, ['claude-haiku-5-5', 'claude-opus-4-5-20251101'])
  assert.equal(normalized.modelOverrides['claude-opus-4-5-20251101'], 'claude-sonnet-4-5')
  assert.equal(config.forcedModelMappingError(normalized), null)
  for (const id of [...defaults.listedModels, 'claude-fable-6-1', 'claude-haiku-5-5-thinking[1m]', 'claude-haiku-5-5[1m]-thinking', 'sonnet', 'sonnet-thinking[1m]']) {
    assert.equal(config.isForcedListedModel(id), true, id)
  }
  for (const id of ['', 'deepseek-3.2', 'claude-unknown-5', 'claude-sonnet-5/path', 'claude-haiku-5?foo']) {
    assert.ok(config.forcedModelMappingError({ ...defaults, listedModels: [id] }), id)
  }
  const rust = readFileSync(join(root, 'src/model/forced_model_mapping.rs'), 'utf8')
  const block = rust.split('pub const DEFAULT_FORCED_LISTED_MODELS: &[&str] = &[')[1].split('];')[0]
  const backend = [...block.matchAll(/"([^"]+)"/g)].map((match) => match[1])
  assert.deepEqual(defaults.listedModels, backend, 'frontend/backend defaults must agree')
  defaults.listedModels.length = 0
  assert.equal(config.defaultForcedModelMapping().listedModels.length, 25)
  console.log('forced model list config: PASS')
} finally {
  rmSync(out, { recursive: true, force: true })
}
