#!/usr/bin/env node

import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')

const sources = [
  {
    name: 'rust',
    file: 'src/model/config.rs',
    pattern: /pub const DEFAULT_TASK_QUALITY_PROMPT: &str = r#"([\s\S]*?)"#;/u,
  },
  {
    name: 'ui',
    file: 'ui/src/lib/runtime-config-defaults.ts',
    pattern: /export const DEFAULT_TASK_QUALITY_PROMPT = `([\s\S]*?)`/u,
  },
  {
    name: 'admin-ui',
    file: 'admin-ui/src/components/runtime-config-panel.tsx',
    pattern: /const DEFAULT_TASK_QUALITY_PROMPT = `([\s\S]*?)`/u,
  },
]

const values = sources.map(({ name, file, pattern }) => {
  const source = fs.readFileSync(path.join(root, file), 'utf8')
  const match = source.match(pattern)
  if (!match) {
    process.stderr.write(`FAIL: cannot extract ${name} DEFAULT_TASK_QUALITY_PROMPT from ${file}\n`)
    process.exit(1)
  }
  return { name, file, value: match[1] }
})

const authority = values[0]
const failures = []
for (const candidate of values.slice(1)) {
  if (candidate.value !== authority.value) {
    failures.push(`${candidate.name} default differs byte-for-byte from Rust authority`)
  }
}

for (const candidate of values) {
  for (const marker of [
    'readHash',
    'editHash',
    'bashHash',
    'Tool results:',
    'Tool results provided',
    'function_results',
  ]) {
    if (candidate.value.includes(marker)) {
      failures.push(`${candidate.name} default primes internal marker ${marker}`)
    }
  }
}

if (failures.length) {
  for (const failure of failures) process.stderr.write(`FAIL: ${failure}\n`)
  process.exit(1)
}

process.stdout.write('PASS: Rust, UI, and Admin UI task-quality defaults match and contain no internal transcript fingerprints.\n')
