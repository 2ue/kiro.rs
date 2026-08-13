import fs from 'node:fs'
import path from 'node:path'

function requiredAbsolutePath(name) {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required and must be an absolute path outside the repository`)
  if (!path.isAbsolute(value)) throw new Error(`${name} must be an absolute path outside the repository`)
  return path.resolve(value)
}

function isWithin(parent, candidate) {
  const relative = path.relative(parent, candidate)
  return relative === '' || (
    relative !== '..'
    && !relative.startsWith(`..${path.sep}`)
    && !path.isAbsolute(relative)
  )
}

function isDirectCargoOutputPath(candidate) {
  const segments = path.resolve(candidate).split(path.sep).filter(Boolean).map((value) => (
    value.toLowerCase()
  ))
  return segments.some((segment, index) => (
    segment === 'target'
    && (segments[index + 1] === 'debug' || segments[index + 1] === 'release')
  ))
}

function requireExternalRealPath(repoRoot, configuredPath, name, expectedType) {
  if (!fs.existsSync(configuredPath)) {
    throw new Error(`${name} does not exist; provide an owned external validation path`)
  }
  const realPath = fs.realpathSync(configuredPath)
  if (name === 'KIRO_RS_BINARY' && (
    isDirectCargoOutputPath(configuredPath) || isDirectCargoOutputPath(realPath)
  )) {
    throw new Error(
      'KIRO_RS_BINARY must be a copied frozen candidate, not target/debug or target/release output',
    )
  }
  const stat = fs.statSync(realPath)
  if (expectedType === 'file' && !stat.isFile()) throw new Error(`${name} must reference a file`)
  if (expectedType === 'directory' && !stat.isDirectory()) {
    throw new Error(`${name} must reference an existing directory`)
  }
  if (isWithin(repoRoot, realPath)) {
    throw new Error(`${name} resolves inside the repository; provide an owned external path`)
  }
  if (expectedType === 'directory' && isWithin(realPath, repoRoot)) {
    throw new Error(`${name} must not contain the repository; provide a dedicated external directory`)
  }
  return realPath
}

export function resolveRuntimeValidationPaths(root) {
  const repoRoot = fs.realpathSync(root)
  return {
    binary: requireExternalRealPath(
      repoRoot,
      requiredAbsolutePath('KIRO_RS_BINARY'),
      'KIRO_RS_BINARY',
      'file',
    ),
    artifactRoot: requireExternalRealPath(
      repoRoot,
      requiredAbsolutePath('KIRO_VALIDATION_ARTIFACT_DIR'),
      'KIRO_VALIDATION_ARTIFACT_DIR',
      'directory',
    ),
  }
}
