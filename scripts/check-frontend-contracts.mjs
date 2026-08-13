import fs from 'node:fs'
import path from 'node:path'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(import.meta.url)
const ts = require(path.join(root, 'ui/node_modules/typescript/lib/typescript.js'))

const uiContractPath = path.join(root, 'ui/src/types/api.ts')
const adminContractPath = path.join(root, 'admin-ui/src/types/api.ts')
const virtualCheckPath = path.join(root, 'scripts/.frontend-contract-check.ts')

const intentionallyUiOnly = new Set(['UsageRouteKindFilter'])
const intentionallyAdminOnly = new Set(['AdminErrorResponse', 'BulkCredentialActionError'])

function exportedTypeNames(filePath) {
  const source = fs.readFileSync(filePath, 'utf8')
  const sourceFile = ts.createSourceFile(filePath, source, ts.ScriptTarget.Latest, true)
  const supportedDeclarations = new Set([
    ts.SyntaxKind.EnumDeclaration,
    ts.SyntaxKind.InterfaceDeclaration,
    ts.SyntaxKind.TypeAliasDeclaration,
  ])

  return new Set(
    sourceFile.statements
      .filter((statement) =>
        supportedDeclarations.has(statement.kind) &&
        statement.name?.text &&
        statement.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.ExportKeyword),
      )
      .map((statement) => statement.name.text),
  )
}

function unexpectedNames(source, target, allowed) {
  return [...source].filter((name) => !target.has(name) && !allowed.has(name)).sort()
}

const uiNames = exportedTypeNames(uiContractPath)
const adminNames = exportedTypeNames(adminContractPath)
const unexpectedUiOnly = unexpectedNames(uiNames, adminNames, intentionallyUiOnly)
const unexpectedAdminOnly = unexpectedNames(adminNames, uiNames, intentionallyAdminOnly)

if (unexpectedUiOnly.length || unexpectedAdminOnly.length) {
  if (unexpectedUiOnly.length) {
    console.error(`Types exported only by ui/src/types/api.ts: ${unexpectedUiOnly.join(', ')}`)
  }
  if (unexpectedAdminOnly.length) {
    console.error(
      `Types exported only by admin-ui/src/types/api.ts: ${unexpectedAdminOnly.join(', ')}`,
    )
  }
  console.error('Share the contract or document an intentional exception in this script.')
  process.exit(1)
}

const sharedNames = [...uiNames].filter((name) => adminNames.has(name)).sort()
const sourceLines = [
  "import type * as Ui from '../ui/src/types/api'",
  "import type * as Admin from '../admin-ui/src/types/api'",
  'type Equal<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false',
  'type Assert<T extends true> = T',
  ...sharedNames.map(
    (name) => `type Check_${name} = Assert<Equal<Ui.${name}, Admin.${name}>>`,
  ),
]
const virtualSource = sourceLines.join('\n')
const compilerOptions = {
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  noEmit: true,
  skipLibCheck: true,
  strict: true,
  target: ts.ScriptTarget.ES2020,
}
const host = ts.createCompilerHost(compilerOptions)
const defaultFileExists = host.fileExists.bind(host)
const defaultGetSourceFile = host.getSourceFile.bind(host)
const defaultReadFile = host.readFile.bind(host)

host.fileExists = (filePath) =>
  path.resolve(filePath) === virtualCheckPath || defaultFileExists(filePath)
host.readFile = (filePath) =>
  path.resolve(filePath) === virtualCheckPath ? virtualSource : defaultReadFile(filePath)
host.getSourceFile = (filePath, languageVersion, ...rest) =>
  path.resolve(filePath) === virtualCheckPath
    ? ts.createSourceFile(filePath, virtualSource, languageVersion, true)
    : defaultGetSourceFile(filePath, languageVersion, ...rest)

const program = ts.createProgram([virtualCheckPath], compilerOptions, host)
const diagnostics = ts.getPreEmitDiagnostics(program)
if (diagnostics.length) {
  const formatHost = {
    getCanonicalFileName: (fileName) => fileName,
    getCurrentDirectory: () => root,
    getNewLine: () => '\n',
  }
  console.error(ts.formatDiagnosticsWithColorAndContext(diagnostics, formatHost))
  console.error('The two frontend API contracts have drifted.')
  process.exit(1)
}

console.log(`Frontend API contracts match across ${sharedNames.length} shared types.`)
