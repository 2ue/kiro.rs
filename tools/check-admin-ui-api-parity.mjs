#!/usr/bin/env node

import fs from 'node:fs'
import path from 'node:path'

const roots = {
  legacy: 'admin-ui/src',
  console: 'admin-ui-daisy/src',
}

function sourceFiles(dir) {
  const entries = fs.readdirSync(dir, { withFileTypes: true })
  return entries.flatMap((entry) => {
    const fullPath = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      return sourceFiles(fullPath)
    }
    return /\.(ts|tsx)$/.test(entry.name) ? [fullPath] : []
  })
}

function normalizeEndpoint(raw) {
  let endpoint = raw.replace(/\$\{[^}]*\}/g, ':param')
  if (endpoint.startsWith('/api/admin/')) {
    endpoint = endpoint.slice('/api/admin'.length)
  }
  return endpoint
}

function endpointsFor(root) {
  const endpoints = new Set()
  const callPattern = /\b(?:api|axios)\s*\.\s*(?:get|post|put|delete|patch)(?:\s*<[\s\S]*?>)?\s*\(\s*(['"`])([\s\S]*?)\1/g

  for (const file of sourceFiles(root)) {
    const source = fs.readFileSync(file, 'utf8')
    for (const match of source.matchAll(callPattern)) {
      const endpoint = normalizeEndpoint(match[2])
      if (endpoint.startsWith('/')) {
        endpoints.add(endpoint)
      }
    }
  }

  return [...endpoints].sort()
}

const legacy = endpointsFor(roots.legacy)
const consoleUi = endpointsFor(roots.console)
const onlyLegacy = legacy.filter((endpoint) => !consoleUi.includes(endpoint))
const onlyConsole = consoleUi.filter((endpoint) => !legacy.includes(endpoint))

if (onlyLegacy.length || onlyConsole.length) {
  console.error('Admin UI API endpoint coverage differs.')
  console.error(JSON.stringify({ onlyLegacy, onlyConsole }, null, 2))
  process.exit(1)
}

console.log(`Admin UI API endpoint coverage matches (${legacy.length} endpoints).`)
