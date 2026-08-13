import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const targets = ['ui/src/types/api.ts', 'admin-ui/src/types/api.ts']
const requiredFields = [
  'maxAttempts',
  'consumed',
  'localAttempts',
  'externalAttempts',
  'mcpAttempts',
  'exhausted',
  'downstreamCommitted',
]

for (const target of targets) {
  const source = fs.readFileSync(path.join(root, target), 'utf8')
  const match = source.match(/export interface InferenceAttemptSnapshot\s*\{([\s\S]*?)\n\}/)
  assert.ok(match, `${target}: missing InferenceAttemptSnapshot`)
  for (const field of requiredFields) {
    assert.match(match[1], new RegExp(`\\b${field}\\s*:\\s*(?:number|boolean)\\b`), `${target}: missing ${field}`)
  }
}

console.log('PASS: both UI contracts expose the explicit MCP attempt channel')
