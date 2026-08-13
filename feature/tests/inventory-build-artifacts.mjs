#!/usr/bin/env node

import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'

const VERSION = 2
const MAX_EXPLICIT_TEMP_ENTRIES = 20000
const MAX_TEMP_DEPTH = 5
const KNOWN_TEMP_CONTAINER = /^(?:kiro(?:[-_.]|$)|cargo(?:[-_.]?target|[-_.])|\.validation[-_.])/i

function usage() {
  process.stdout.write(`Usage: node feature/tests/inventory-build-artifacts.mjs [options]\n\n`)
  process.stdout.write(`Read-only inventory of Cargo build targets and reservations.\n\n`)
  process.stdout.write(`Options:\n`)
  process.stdout.write(`  --gate              Exit nonzero when release blockers exist\n`)
  process.stdout.write(`  --json              Emit one JSON document\n`)
  process.stdout.write(`  --repo-root PATH    Override repository root for an isolated smoke test\n`)
  process.stdout.write(`  --state-dir PATH    Override reservation state directory\n`)
  process.stdout.write(`  --temp-root PATH    Add a private-temp root to scan; repeatable\n`)
  process.stdout.write(`  --only-temp-roots   Do not scan the operating-system temp directory\n`)
  process.stdout.write(`  --no-docker         Skip read-only Docker disk inspection\n`)
}

function parseArgs(argv) {
  const options = {
    gate: false,
    json: false,
    repoRoot: null,
    stateDir: null,
    tempRoots: [],
    onlyTempRoots: false,
    docker: true,
  }
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index]
    if (arg === '--gate') options.gate = true
    else if (arg === '--json') options.json = true
    else if (arg === '--only-temp-roots') options.onlyTempRoots = true
    else if (arg === '--no-docker') options.docker = false
    else if (arg === '--repo-root' || arg === '--state-dir' || arg === '--temp-root') {
      const value = argv[index + 1]
      if (!value) throw new Error(`${arg} requires a path`)
      index += 1
      if (arg === '--repo-root') options.repoRoot = value
      else if (arg === '--state-dir') options.stateDir = value
      else options.tempRoots.push(value)
    } else if (arg === '--help' || arg === '-h') {
      usage()
      process.exit(0)
    } else {
      throw new Error(`unknown option: ${arg}`)
    }
  }
  return options
}

function run(command, args, timeout = 5000) {
  const result = spawnSync(command, args, {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout,
    maxBuffer: 8 * 1024 * 1024,
  })
  return {
    ok: result.status === 0 && !result.error,
    stdout: result.stdout || '',
    status: result.status,
    timedOut: result.error?.code === 'ETIMEDOUT',
  }
}

function canonical(existingPath) {
  const absolute = path.resolve(existingPath)
  try {
    return fs.realpathSync.native(absolute)
  } catch {
    return absolute
  }
}

function readFirst(file) {
  try {
    return fs.readFileSync(file, 'utf8').split(/\r?\n/, 1)[0]
  } catch {
    return ''
  }
}

function isDirectory(directory) {
  try {
    return fs.statSync(directory).isDirectory() && !fs.lstatSync(directory).isSymbolicLink()
  } catch {
    return false
  }
}

function exists(file) {
  try {
    fs.accessSync(file, fs.constants.F_OK)
    return true
  } catch {
    return false
  }
}

function digest(value) {
  return crypto.createHash('sha256').update(value).digest('hex').slice(0, 12)
}

function safeName(value) {
  const sanitized = value.replace(/[^A-Za-z0-9._-]/g, '_')
  return sanitized.slice(0, 80) || 'unnamed'
}

function processStart(pid) {
  const result = run('ps', ['-p', String(pid), '-o', 'lstart='], 2000)
  return result.ok ? result.stdout.trim().replace(/\s+/g, ' ') : ''
}

function ownerActive(pidText, recordedStart) {
  if (!/^[0-9]+$/.test(pidText)) return false
  const pid = Number(pidText)
  try {
    process.kill(pid, 0)
  } catch {
    return false
  }
  const currentStart = processStart(pid)
  if (!recordedStart || !currentStart) return true
  return currentStart === recordedStart.replace(/\s+/g, ' ').trim()
}

function looksLikeCargoTarget(directory) {
  if (!isDirectory(directory)) return false
  if (path.basename(directory).startsWith('.validation-build-')) return true
  if (exists(path.join(directory, '.rustc_info.json'))) return true
  if (exists(path.join(directory, 'CACHEDIR.TAG'))) return true
  for (const profile of ['debug', 'release']) {
    if (exists(path.join(directory, profile, '.fingerprint'))) return true
    if (exists(path.join(directory, profile, 'deps'))) return true
    if (exists(path.join(directory, profile, 'build'))) return true
  }
  return false
}

function directorySizeKib(directory) {
  const result = run('du', ['-sk', directory], 5000)
  if (!result.ok) return null
  const match = result.stdout.match(/^([0-9]+)/)
  return match ? Number(match[1]) : null
}

function gitOutput(repoRoot, args) {
  return run('git', ['-C', repoRoot, ...args], 5000)
}

function discoverWorktrees(repoRoot) {
  const result = gitOutput(repoRoot, ['worktree', 'list', '--porcelain'])
  if (!result.ok) return [{ path: repoRoot, id: digest(repoRoot), primary: true }]
  const worktrees = []
  for (const line of result.stdout.split(/\r?\n/)) {
    if (!line.startsWith('worktree ')) continue
    const worktreePath = canonical(line.slice('worktree '.length))
    worktrees.push({
      path: worktreePath,
      id: digest(worktreePath),
      primary: worktreePath === repoRoot,
    })
  }
  if (!worktrees.some((entry) => entry.path === repoRoot)) {
    worktrees.unshift({ path: repoRoot, id: digest(repoRoot), primary: true })
  }
  return worktrees
}

function defaultStateDir(repoRoot) {
  const result = gitOutput(repoRoot, ['rev-parse', '--git-common-dir'])
  if (!result.ok) return path.join(repoRoot, '.git', 'kiro-validation-build-state')
  const value = result.stdout.trim()
  const common = canonical(path.isAbsolute(value) ? value : path.join(repoRoot, value))
  return path.join(common, 'kiro-validation-build-state')
}

function readReservations(stateDir) {
  const reservations = []
  if (!isDirectory(stateDir)) return reservations
  let entries = []
  try {
    entries = fs.readdirSync(stateDir, { withFileTypes: true })
  } catch {
    return [{ valid: false, id: 'state-unreadable', active: false, target: '' }]
  }
  for (const entry of entries) {
    if (!entry.name.startsWith('.reservation-') || entry.name.startsWith('.reservation-tmp-')) continue
    const directory = path.join(stateDir, entry.name)
    if (!entry.isDirectory() || entry.isSymbolicLink()) {
      reservations.push({ valid: false, id: digest(directory), active: false, target: '' })
      continue
    }
    const id = readFirst(path.join(directory, 'reservation_id'))
    const pid = readFirst(path.join(directory, 'owner_pid'))
    const start = readFirst(path.join(directory, 'owner_start'))
    const created = readFirst(path.join(directory, 'created_epoch'))
    const reserved = readFirst(path.join(directory, 'reserved_kib'))
    const filesystem = readFirst(path.join(directory, 'filesystem_id'))
    const target = readFirst(path.join(directory, 'target_dir'))
    const scope = readFirst(path.join(directory, 'scope'))
    const valid = /^[A-Za-z0-9._-]+$/.test(id)
      && entry.name === `.reservation-${id}`
      && /^[0-9]+$/.test(pid)
      && Boolean(start)
      && /^[0-9]+$/.test(created)
      && /^[0-9]+$/.test(reserved)
      && Boolean(filesystem)
      && path.isAbsolute(target)
      && /^[a-z0-9][a-z0-9._-]{0,63}$/.test(scope)
    reservations.push({
      valid,
      id: valid ? id : digest(directory),
      pid: valid ? Number(pid) : null,
      active: valid ? ownerActive(pid, start) : false,
      target: valid ? canonical(target) : '',
      reservedKib: valid ? Number(reserved) : null,
      scope: valid ? scope : 'invalid',
    })
  }
  return reservations
}

function scanScopedChildren(targetRoot, addCandidate, source) {
  if (!isDirectory(targetRoot)) return
  let entries = []
  try {
    entries = fs.readdirSync(targetRoot, { withFileTypes: true })
  } catch {
    return
  }
  for (const entry of entries) {
    if (!entry.name.startsWith('.validation-build-')) continue
    if (entry.isDirectory() && !entry.isSymbolicLink()) {
      addCandidate(path.join(targetRoot, entry.name), `${source}-scoped`)
    }
  }
}

function scanTempTree(root, addCandidate, scanState, source) {
  const canonicalRoot = canonical(root)
  if (!isDirectory(canonicalRoot)) return
  const queue = [{ directory: canonicalRoot, depth: 0 }]
  while (queue.length > 0) {
    const { directory, depth } = queue.shift()
    if (scanState.explicitEntries >= MAX_EXPLICIT_TEMP_ENTRIES) {
      scanState.truncated = true
      return
    }
    let entries = []
    try {
      entries = fs.readdirSync(directory, { withFileTypes: true })
    } catch {
      continue
    }
    scanState.entries += entries.length
    scanState.explicitEntries += entries.length
    for (const entry of entries) {
      if (!entry.isDirectory() || entry.isSymbolicLink()) continue
      if (['.git', 'node_modules', '.Trash'].includes(entry.name)) continue
      const child = path.join(directory, entry.name)
      const candidateName = entry.name === 'target'
        || entry.name.startsWith('.validation-build-')
        || /^cargo[-_.]?target/i.test(entry.name)
        || /^kiro[-_.].*target/i.test(entry.name)
      if (candidateName && looksLikeCargoTarget(child)) {
        addCandidate(child, source)
        continue
      }
      if (depth + 1 < MAX_TEMP_DEPTH) queue.push({ directory: child, depth: depth + 1 })
    }
  }
}

function scanTempRoot(root, addCandidate, scanState, explicit) {
  const canonicalRoot = canonical(root)
  if (!isDirectory(canonicalRoot)) return
  scanState.roots += 1

  if (looksLikeCargoTarget(canonicalRoot)) {
    addCandidate(canonicalRoot, explicit ? 'explicit-private-temp' : 'private-temp')
    return
  }
  if (explicit) {
    scanTempTree(canonicalRoot, addCandidate, scanState, 'explicit-private-temp')
    return
  }

  let entries = []
  try {
    entries = fs.readdirSync(canonicalRoot, { withFileTypes: true })
  } catch {
    scanState.unreadable += 1
    return
  }
  scanState.entries += entries.length
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink()) continue
    const child = path.join(canonicalRoot, entry.name)
    if (looksLikeCargoTarget(child)) {
      addCandidate(child, 'private-temp')
      continue
    }
    if (KNOWN_TEMP_CONTAINER.test(entry.name)) {
      scanTempTree(child, addCandidate, scanState, 'private-temp')
    }
  }
}

function classifyProcess(text) {
  const lower = text.toLowerCase()
  if (lower.includes('rust-analyzer')) return 'rust-analyzer'
  if (/(^|[/\s])cargo([\s]|$)/.test(lower)) return 'cargo'
  if (/(^|[/\s])rustc([\s]|$)/.test(lower)) return 'rustc'
  if (lower.includes('kiro-rs')) return 'kiro-runtime'
  if (lower.includes('node')) return 'runtime-helper'
  if (lower.includes('bash') || lower.includes('zsh') || lower.includes('/sh ')) return 'shell-wrapper'
  return 'other-target-reference'
}

function inferCargoTargetFromPath(filePath) {
  const cleaned = filePath.replace(/\s+\(deleted\)$/, '')
  if (!path.isAbsolute(cleaned)) return null
  if (!/(?:^|\/)(?:target|\.validation-build-[^/]*|cargo[-_.]?target[^/]*|kiro[-_.][^/]*target[^/]*)(?:\/|$)/i.test(cleaned)) {
    return null
  }
  let current = canonical(cleaned)
  try {
    if (!fs.statSync(current).isDirectory()) current = path.dirname(current)
  } catch {
    current = path.dirname(current)
  }
  while (current !== path.dirname(current)) {
    const name = path.basename(current)
    const namedTarget = name === 'target'
      || name.startsWith('.validation-build-')
      || /^cargo[-_.]?target/i.test(name)
      || /^kiro[-_.].*target/i.test(name)
    if ((namedTarget || looksLikeCargoTarget(current)) && looksLikeCargoTarget(current)) {
      return current
    }
    current = path.dirname(current)
  }
  return null
}

function discoverOpenProcessEntries(addCandidate, candidateAllowed) {
  const entries = []
  if (process.platform === 'linux' && isDirectory('/proc')) {
    let procEntries = []
    try {
      procEntries = fs.readdirSync('/proc', { withFileTypes: true })
    } catch {
      return { complete: false, method: 'proc-unreadable', entries }
    }
    for (const entry of procEntries) {
      if (!entry.isDirectory() || !/^[0-9]+$/.test(entry.name)) continue
      const pid = Number(entry.name)
      if (pid === process.pid) continue
      const command = readFirst(`/proc/${pid}/comm`)
      for (const descriptor of ['cwd', 'exe']) {
        let resolved
        try {
          resolved = fs.readlinkSync(`/proc/${pid}/${descriptor}`)
        } catch {
          continue
        }
        const candidate = inferCargoTargetFromPath(resolved)
        if (candidate && candidateAllowed(candidate)) addCandidate(candidate, 'live-process-artifact')
        entries.push({ pid, command, descriptor, path: canonical(resolved) })
      }
    }
    return { complete: true, method: 'proc-cwd-exe', entries }
  }

  // lsof is read-only. Raw commands and paths are consumed only in memory and
  // are never included in either the human or JSON report.
  const lsof = run('lsof', ['-nP', '-Fpcfn', '-d', 'cwd,txt'], 8000)
  if ((!lsof.ok && !lsof.stdout) || lsof.timedOut) {
    return { complete: false, method: lsof.timedOut ? 'lsof-timeout' : 'lsof-unavailable', entries }
  }
  let pid = null
  let command = ''
  let descriptor = ''
  for (const line of lsof.stdout.split(/\r?\n/)) {
    const prefix = line[0]
    const value = line.slice(1)
    if (prefix === 'p') {
      pid = /^[0-9]+$/.test(value) ? Number(value) : null
      command = ''
      descriptor = ''
    } else if (prefix === 'c') {
      command = value
    } else if (prefix === 'f') {
      descriptor = value
    } else if (prefix === 'n' && pid && pid !== process.pid) {
      const cleaned = value.replace(/\s+\(deleted\)$/, '')
      if (!path.isAbsolute(cleaned)) continue
      const resolved = canonical(cleaned)
      const candidate = inferCargoTargetFromPath(resolved)
      if (candidate && candidateAllowed(candidate)) addCandidate(candidate, 'live-process-artifact')
      entries.push({ pid, command, descriptor, path: resolved })
    }
  }
  return { complete: true, method: 'lsof-cwd-txt', entries }
}

function isInside(candidatePath, parentPath) {
  return candidatePath === parentPath || candidatePath.startsWith(`${parentPath}${path.sep}`)
}

function openPathReferencesTarget(openPath, target) {
  if (!isInside(openPath, target.path)) return false
  return !target.excludedPaths.some((excluded) => isInside(openPath, excluded))
}

function relativeTextReferencesTarget(processText, target, cwd) {
  if (!cwd) return false
  for (const rawToken of processText.split(/\s+/)) {
    let token = rawToken.replace(/^["']+|["',;]+$/g, '')
    const equals = token.indexOf('=')
    if (equals >= 0) token = token.slice(equals + 1)
    if (!token.includes('/')) continue
    const resolved = canonical(path.isAbsolute(token) ? token : path.resolve(cwd, token))
    if (openPathReferencesTarget(resolved, target)) return true
  }
  return false
}

function textReferencesTarget(processText, target, cwd) {
  const normalized = processText.replace(/\/{2,}/g, '/')
  const targetMatches = target.matchPaths.some((candidatePath) => (
    processText.includes(candidatePath)
    || normalized.includes(candidatePath.replace(/\/{2,}/g, '/'))
  ))
  if (!targetMatches) return relativeTextReferencesTarget(processText, target, cwd)

  const excludedMatch = target.excludedPaths.some((excluded) => (
    processText.includes(excluded)
    || normalized.includes(excluded.replace(/\/{2,}/g, '/'))
  ))
  if (!excludedMatch) return true

  // A registered source worktree may live below the repository's target/
  // directory. Only explicit Cargo artifact profiles override that exclusion.
  return target.matchPaths.some((candidatePath) => [
    `${candidatePath}${path.sep}debug`,
    `${candidatePath}${path.sep}release`,
    `${candidatePath}${path.sep}.validation-build-`,
  ].some((artifactPath) => processText.includes(artifactPath) || normalized.includes(artifactPath)))
}

function discoverProcessReferences(targets, openInspection) {
  const references = new Map()
  const cwdByPid = new Map(
    openInspection.entries
      .filter((entry) => entry.descriptor === 'cwd')
      .map((entry) => [entry.pid, entry.path]),
  )

  function addReference(target, pid, processText) {
    const key = `${target.id}:${pid}`
    const candidate = { targetId: target.id, pid, classification: classifyProcess(processText) }
    const previous = references.get(key)
    if (!previous || previous.classification === 'other-target-reference') references.set(key, candidate)
  }

  const ps = run('ps', ['-wwaxo', 'pid=,comm=,args='], 5000)
  if (ps.ok) {
    for (const line of ps.stdout.split(/\r?\n/)) {
      const match = line.match(/^\s*([0-9]+)\s+(.*)$/)
      if (!match) continue
      const pid = Number(match[1])
      if (pid === process.pid) continue
      const processText = match[2]
      for (const target of targets) {
        if (textReferencesTarget(processText, target, cwdByPid.get(pid))) {
          addReference(target, pid, processText)
        }
      }
    }
  }

  for (const entry of openInspection.entries) {
    for (const target of targets) {
      if (openPathReferencesTarget(entry.path, target)) {
        addReference(target, entry.pid, entry.command)
      }
    }
  }
  return {
    complete: ps.ok && openInspection.complete,
    ps: ps.ok ? 'complete' : 'unavailable',
    openFiles: openInspection.method,
    references: [...references.values()].sort((a, b) => a.pid - b.pid || a.targetId.localeCompare(b.targetId)),
  }
}

function inspectDocker(enabled) {
  if (!enabled) return { status: 'skipped', cleanup: 'manual-only', rows: [] }
  const result = run('docker', ['system', 'df', '--format', '{{.Type}}\t{{.Size}}\t{{.Reclaimable}}'], 5000)
  if (!result.ok) {
    return {
      status: result.timedOut ? 'timed-out' : 'unavailable',
      cleanup: 'manual-only',
      rows: [],
    }
  }
  const rows = []
  for (const line of result.stdout.split(/\r?\n/)) {
    if (!line.trim()) continue
    const [type = '', size = '', reclaimable = ''] = line.split('\t')
    rows.push({
      type: safeName(type),
      size: safeName(size),
      reclaimable: safeName(reclaimable),
    })
  }
  return { status: 'inspected-read-only', cleanup: 'manual-only', rows }
}

function locatorFor(candidate, repoRoot, worktrees, tempRoots) {
  if (candidate.path === path.join(repoRoot, 'target')) return '<repo>/target'
  if (candidate.path.startsWith(`${repoRoot}${path.sep}`)) {
    return `<repo>/${safeName(path.relative(repoRoot, candidate.path))}`
  }
  for (const worktree of worktrees) {
    if (candidate.path === path.join(worktree.path, 'target')) return `<worktree:${worktree.id}>/target`
    if (candidate.path.startsWith(`${worktree.path}${path.sep}`)) {
      return `<worktree:${worktree.id}>/${safeName(path.relative(worktree.path, candidate.path))}`
    }
  }
  for (const root of tempRoots) {
    if (candidate.path.startsWith(`${root}${path.sep}`)) {
      return `<temp:${digest(root)}>/${safeName(path.basename(candidate.path))}`
    }
  }
  return `<external:${digest(candidate.path)}>/${safeName(path.basename(candidate.path))}`
}

function main() {
  let options
  try {
    options = parseArgs(process.argv.slice(2))
  } catch (error) {
    process.stderr.write(`inventory argument error: ${error.message}\n`)
    process.exit(64)
  }

  const cwdRepo = run('git', ['rev-parse', '--show-toplevel'], 3000)
  const repoRoot = canonical(options.repoRoot || (cwdRepo.ok ? cwdRepo.stdout.trim() : process.cwd()))
  const stateDir = canonical(options.stateDir || defaultStateDir(repoRoot))
  const worktrees = discoverWorktrees(repoRoot)
  const reservations = readReservations(stateDir)
  const candidates = new Map()

  function addCandidate(candidatePath, source) {
    const lexical = path.resolve(candidatePath)
    const resolved = canonical(candidatePath)
    const platformAliases = [lexical, resolved]
    if (process.platform === 'darwin' && resolved.startsWith('/private/var/')) {
      platformAliases.push(resolved.slice('/private'.length))
    }
    if (!isDirectory(resolved)) return
    const existing = candidates.get(resolved)
    if (existing) {
      existing.sources.add(source)
      for (const alias of platformAliases) existing.matchPaths.add(alias)
      return
    }
    candidates.set(resolved, {
      path: resolved,
      matchPaths: new Set(platformAliases),
      sources: new Set([source]),
    })
  }

  const repoTarget = path.join(repoRoot, 'target')
  if (looksLikeCargoTarget(repoTarget)) addCandidate(repoTarget, 'repo-default')
  scanScopedChildren(repoTarget, addCandidate, 'repo-default')

  for (const worktree of worktrees) {
    if (worktree.path === repoRoot) continue
    const target = path.join(worktree.path, 'target')
    if (looksLikeCargoTarget(target)) addCandidate(target, 'worktree-default')
    scanScopedChildren(target, addCandidate, 'worktree-default')
  }

  if (process.env.CARGO_TARGET_DIR && looksLikeCargoTarget(process.env.CARGO_TARGET_DIR)) {
    addCandidate(process.env.CARGO_TARGET_DIR, 'explicit-environment')
  }
  for (const reservation of reservations) {
    if (reservation.valid && reservation.target && isDirectory(reservation.target)) {
      addCandidate(reservation.target, 'reservation-record')
    }
  }

  const defaultTempRoots = []
  if (!options.onlyTempRoots) {
    defaultTempRoots.push(canonical(os.tmpdir()))
    if (isDirectory('/tmp')) defaultTempRoots.push(canonical('/tmp'))
  }
  const explicitTempRoots = options.tempRoots.map((root) => canonical(root))
  const uniqueDefaultTempRoots = [...new Set(defaultTempRoots)]
  const uniqueExplicitTempRoots = [...new Set(explicitTempRoots)]
  const uniqueTempRoots = [...new Set([...uniqueDefaultTempRoots, ...uniqueExplicitTempRoots])]
  const scanState = { entries: 0, explicitEntries: 0, roots: 0, unreadable: 0, truncated: false }
  for (const root of uniqueDefaultTempRoots) scanTempRoot(root, addCandidate, scanState, false)
  for (const root of uniqueExplicitTempRoots) scanTempRoot(root, addCandidate, scanState, true)

  const processCandidateRoots = [
    repoRoot,
    ...worktrees.map((worktree) => worktree.path),
    ...uniqueTempRoots,
    ...reservations.filter((entry) => entry.valid && entry.target).map((entry) => entry.target),
  ]
  const openInspection = discoverOpenProcessEntries(
    addCandidate,
    (candidate) => processCandidateRoots.some((root) => isInside(candidate, root)),
  )

  const reservationByTarget = new Map(
    reservations.filter((entry) => entry.valid && entry.target).map((entry) => [entry.target, entry]),
  )
  const targets = []
  for (const candidate of candidates.values()) {
    const markerPid = readFirst(path.join(candidate.path, '.owner-pid'))
    const markerStart = readFirst(path.join(candidate.path, '.owner-start'))
    const markerReservation = readFirst(path.join(candidate.path, '.owner-reservation-id'))
    const matchingReservation = reservationByTarget.get(candidate.path)
    const ownerIsActive = ownerActive(markerPid, markerStart)
    const sources = [...candidate.sources].sort()
    let classification
    if (markerReservation && matchingReservation?.id === markerReservation) {
      classification = matchingReservation.active ? 'scoped-active' : 'scoped-stale'
    } else if (markerPid || markerStart || markerReservation) {
      classification = ownerIsActive ? 'scoped-active-unreserved' : 'scoped-stale-unreserved'
    } else if (sources.includes('repo-default')) {
      classification = 'unmanaged-repo-cargo-target'
    } else if (sources.includes('worktree-default')) {
      classification = 'unmanaged-worktree-cargo-target'
    } else if (sources.includes('explicit-environment')) {
      classification = 'unmanaged-explicit-cargo-target'
    } else {
      classification = 'unknown-private-temp-cargo-target'
    }
    targets.push({
      id: digest(candidate.path),
      path: candidate.path,
      matchPaths: [...candidate.matchPaths],
      excludedPaths: worktrees
        .map((worktree) => worktree.path)
        .filter((worktreePath) => worktreePath !== candidate.path && isInside(worktreePath, candidate.path)),
      locator: '',
      classification,
      sizeKib: directorySizeKib(candidate.path),
      sources,
    })
  }
  targets.sort((a, b) => a.id.localeCompare(b.id))
  for (const target of targets) target.locator = locatorFor(target, repoRoot, worktrees, uniqueTempRoots)

  const processInspection = discoverProcessReferences(targets, openInspection)
  const processReferences = processInspection.references
  const invalidReservations = reservations.filter((entry) => !entry.valid).length
  const blockers = targets.length
    + processReferences.length
    + invalidReservations
    + (scanState.truncated ? 1 : 0)
    + scanState.unreadable
    + (processInspection.complete ? 0 : 1)
  const docker = inspectDocker(options.docker)
  const report = {
    version: VERSION,
    mode: 'read-only',
    summary: {
      targetCount: targets.length,
      reservationCount: reservations.length,
      invalidReservationCount: invalidReservations,
      targetProcessCount: processReferences.length,
      tempEntriesInspected: scanState.entries,
      tempRootsInspected: scanState.roots,
      tempRootsUnreadable: scanState.unreadable,
      tempScanTruncated: scanState.truncated,
      processInspectionComplete: processInspection.complete,
      blockerCount: blockers,
      releaseGate: blockers === 0 ? 'pass' : 'fail',
    },
    targets: targets.map(({
      path: _path,
      matchPaths: _matchPaths,
      excludedPaths: _excludedPaths,
      ...target
    }) => target),
    reservations: reservations.map((entry) => ({
      id: entry.id,
      valid: entry.valid,
      active: entry.active,
      scope: entry.scope || 'invalid',
      reservedKib: entry.reservedKib,
    })),
    processes: processReferences,
    processInspection: {
      complete: processInspection.complete,
      ps: processInspection.ps,
      openFiles: processInspection.openFiles,
    },
    docker,
    cleanupPolicy: 'report-only; delete scoped targets through run-cargo-scoped.sh; inspect Docker manually',
  }

  if (options.json) {
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`)
  } else {
    process.stdout.write(`build-artifact-inventory version=${VERSION} mode=read-only targets=${targets.length} reservations=${reservations.length} target_processes=${processReferences.length} blockers=${blockers}\n`)
    for (const target of report.targets) {
      const size = target.sizeKib === null ? 'unknown' : target.sizeKib
      process.stdout.write(`target id=${target.id} location=${target.locator} classification=${target.classification} size_kib=${size}\n`)
    }
    for (const reservation of report.reservations) {
      process.stdout.write(`reservation id=${safeName(reservation.id)} valid=${reservation.valid} active=${reservation.active} scope=${safeName(reservation.scope)} reserved_kib=${reservation.reservedKib ?? 'unknown'}\n`)
    }
    for (const reference of report.processes) {
      process.stdout.write(`target-process target_id=${reference.targetId} pid=${reference.pid} classification=${reference.classification}\n`)
    }
    process.stdout.write(`process-inspection complete=${processInspection.complete} ps=${processInspection.ps} open_files=${processInspection.openFiles}\n`)
    process.stdout.write(`temp-scan roots=${scanState.roots} entries=${scanState.entries} unreadable=${scanState.unreadable} truncated=${scanState.truncated} strategy=bounded-known-prefixes\n`)
    process.stdout.write(`docker status=${docker.status} cleanup=${docker.cleanup} hint=docker-system-df-and-builder-prune-require-manual-review\n`)
    process.stdout.write(`release-gate result=${report.summary.releaseGate}\n`)
  }

  if (options.gate && blockers > 0) process.exit(75)
}

main()
