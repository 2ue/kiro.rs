#!/usr/bin/env node

import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const require = createRequire(import.meta.url)
const ts = require(path.join(root, 'ui/node_modules/typescript/lib/typescript.js'))

const surfaces = [
  {
    file: 'ui/src/features/runtime/runtime-page.tsx',
    setters: ['setPromptSteering', 'setPromptSteeringText', 'setPromptSteeringToggle', 'setChunkedWriteSteering'],
  },
  {
    file: 'admin-ui/src/components/runtime-config-panel.tsx',
    setters: ['updatePromptSteering', 'updatePromptTextBlock', 'updatePromptToggle', 'updateChunkedWrite'],
  },
]

const failures = []

for (const surface of surfaces) {
  const absolute = path.join(root, surface.file)
  const source = fs.readFileSync(absolute, 'utf8')
  const sourceFile = ts.createSourceFile(absolute, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX)
  const declarations = new Map()

  function visit(node) {
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer) {
      declarations.set(node.name.text, node.initializer)
    }
    ts.forEachChild(node, visit)
  }
  visit(sourceFile)

  for (const setter of surface.setters) {
    const declaration = declarations.get(setter)
    if (!declaration) {
      failures.push(`${surface.file}: missing prompt setter ${setter}`)
      continue
    }
    const text = declaration.getText(sourceFile)
    if (text.includes('bodyConversion')) {
      failures.push(`${surface.file}: ${setter} still mutates bodyConversion`)
    }
    if (!text.includes('promptSteering')) {
      failures.push(`${surface.file}: ${setter} does not mutate promptSteering`)
    }
  }

  const saveBody = source.match(/bodyConversion:\s*normalizeBodyConversion\(draft\.bodyConversion\)/g)?.length ?? 0
  const savePrompt = source.match(/promptSteering:\s*normalizePromptSteering\(draft\.promptSteering\)/g)?.length ?? 0
  if (saveBody !== 1) {
    failures.push(`${surface.file}: expected one independent bodyConversion save, found ${saveBody}`)
  }
  if (savePrompt !== 1) {
    failures.push(`${surface.file}: expected one independent promptSteering save, found ${savePrompt}`)
  }

  if (!source.includes('总开关。关闭后不会注入语言约束、任务质量、tool_choice、thinking 或分块写入提示')) {
    failures.push(`${surface.file}: operator master wording does not state the total prompt gate contract`)
  }
}

if (failures.length) {
  for (const failure of failures) process.stderr.write(`FAIL: ${failure}\n`)
  process.exit(1)
}

process.stdout.write(
  `PASS: ${surfaces.length} UI surfaces keep prompt master state separate from body conversion state and document the total prompt gate.\n`,
)
