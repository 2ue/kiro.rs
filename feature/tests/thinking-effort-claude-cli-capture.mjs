#!/usr/bin/env node

import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import { performance } from 'node:perf_hooks'

import { validationChildEnvironment } from './validation-child-env.mjs'

const ROOT = path.resolve(import.meta.dirname, '../..')
const CLAUDE = resolveExecutable(process.env.KIRO_CLAUDE_BINARY || 'claude')
const ROUNDS = Number.parseInt(process.env.KIRO_THINKING_CAPTURE_ROUNDS || '5', 10)
const EFFORTS = ['absent', 'low', 'medium', 'high', 'xhigh', 'max']

if (ROUNDS !== 5) {
  throw new Error('KIRO_THINKING_CAPTURE_ROUNDS must be exactly 5 for the A09/D07 capture gate')
}

const RUN_ID = `thinking-effort-capture-${Date.now()}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const FAKE_KEY = `sk-ant-api03-${crypto.randomBytes(32).toString('hex')}`
const ACTIVE_CHILDREN = new Set()
const MAX_CAPTURE_BYTES = 16 * 1024 * 1024
let fakeServer = null
let fakePort = null
let activeCaseId = null
let cleanupPromise = null
let shutdownExitCode = null

class ShutdownRequested extends Error {}

function throwIfShutdownRequested() {
  if (shutdownExitCode !== null) throw new ShutdownRequested()
}

function resolveExecutable(command) {
  if (path.isAbsolute(command)) {
    if (!fs.existsSync(command)) throw new Error(`Claude binary does not exist: ${command}`)
    return command
  }
  const candidates = [
    process.env.VOLTA_HOME ? path.join(process.env.VOLTA_HOME, 'bin', command) : null,
    path.join(os.homedir(), '.volta', 'bin', command),
    ...String(process.env.PATH || '')
      .split(path.delimiter)
      .filter(Boolean)
      .map((directory) => path.join(directory, command)),
  ].filter(Boolean)
  for (const candidate of candidates) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK)
      return candidate
    } catch {}
  }
  const result = spawnSync('/usr/bin/which', [command], {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
  })
  const resolved = String(result.stdout || '').trim().split(/\r?\n/)[0]
  if (result.status !== 0 || !path.isAbsolute(resolved)) {
    throw new Error(`unable to resolve Claude binary: ${command}`)
  }
  return resolved
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

function commandOutput(command, args) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  })
  if (result.status !== 0) {
    throw new Error(`${command} exited ${result.status}: ${String(result.stderr || '').trim().slice(0, 300)}`)
  }
  return String(result.stdout || '').trim()
}

function listeningPids(port) {
  if (!port) return []
  const result = spawnSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
  })
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`lsof failed while checking port ${port}`)
  }
  return String(result.stdout || '')
    .split(/\s+/)
    .filter(Boolean)
    .map(Number)
    .sort((left, right) => left - right)
}

function processGroupPids(processGroupId) {
  const pids = tryProcessGroupPids(processGroupId)
  if (pids === null) throw new Error(`ps failed while checking process group ${processGroupId}`)
  return pids
}

function tryProcessGroupPids(processGroupId) {
  const result = spawnSync('ps', ['-axo', 'pid=,pgid='], {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  })
  if (result.status !== 0 || result.error) return null
  return String(result.stdout || '')
    .split(/\r?\n/)
    .map((line) => line.trim().split(/\s+/).map(Number))
    .filter(([pid, pgid]) => Number.isInteger(pid) && pgid === processGroupId)
    .map(([pid]) => pid)
    .sort((left, right) => left - right)
}

async function waitForProcessGroupExit(processGroupId, timeoutMs) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const pids = tryProcessGroupPids(processGroupId)
    if (pids !== null && pids.length === 0) return true
    await new Promise((resolve) => setTimeout(resolve, 25))
  }
  const pids = tryProcessGroupPids(processGroupId)
  return pids !== null && pids.length === 0
}

async function stopChild(child) {
  if (!child) return
  const processGroupId = child.pid
  const childExited = () => child.exitCode !== null || child.signalCode !== null
  const initialPids = tryProcessGroupPids(processGroupId)
  if (initialPids === null || initialPids.length > 0) {
    try {
      process.kill(-processGroupId, 'SIGTERM')
    } catch {
      if (!childExited()) child.kill('SIGTERM')
    }
  }
  const exited = await waitForProcessGroupExit(processGroupId, 5_000)
  const afterTermPids = tryProcessGroupPids(processGroupId)
  if (!exited && (afterTermPids === null || afterTermPids.length > 0)) {
    try {
      process.kill(-processGroupId, 'SIGKILL')
    } catch {
      if (!childExited()) child.kill('SIGKILL')
    }
    if (!await waitForProcessGroupExit(processGroupId, 5_000)) {
      throw new Error(`Claude process group ${processGroupId} did not exit during cleanup`)
    }
  }
  if (!childExited()) {
    await Promise.race([
      new Promise((resolve) => child.once('exit', resolve)),
      new Promise((resolve) => setTimeout(resolve, 1_000)),
    ])
  }
  const finalPids = tryProcessGroupPids(processGroupId)
  if ((finalPids !== null && finalPids.length === 0) || childExited()) {
    if (child) ACTIVE_CHILDREN.delete(child)
  }
}

async function cleanupOwnedRuntime() {
  if (cleanupPromise) return cleanupPromise
  cleanupPromise = (async () => {
    for (const child of [...ACTIVE_CHILDREN]) await stopChild(child)
    if (fakeServer?.listening) {
      await new Promise((resolve) => fakeServer.close(resolve))
    }
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    return {
      childrenStopped: ACTIVE_CHILDREN.size === 0,
      portReleased: fakePort === null || listeningPids(fakePort).length === 0,
      tempRemoved: !fs.existsSync(TEMP_ROOT),
    }
  })()
  return cleanupPromise
}

function selectedRequestFacts(bodyBuffer, request) {
  const body = JSON.parse(bodyBuffer.toString('utf8'))
  assert.equal(typeof body, 'object')
  assert.notEqual(body, null)
  assert.equal(Array.isArray(body), false)
  return {
    model: body.model ?? null,
    stream: body.stream ?? null,
    maxTokens: body.max_tokens ?? null,
    thinking: body.thinking ?? null,
    outputConfig: body.output_config ?? null,
    toolCount: Array.isArray(body.tools) ? body.tools.length : 0,
    toolChoice: body.tool_choice ?? null,
    topLevelKeys: Object.keys(body).sort(),
    metadataKeys: body.metadata && typeof body.metadata === 'object'
      ? Object.keys(body.metadata).sort()
      : [],
    contentEncoding: String(request.headers['content-encoding'] || ''),
    bodyBytes: bodyBuffer.length,
    bodySha256: sha256(bodyBuffer),
  }
}

function ssePayload(model, requestId) {
  const events = [
    ['message_start', {
      type: 'message_start',
      message: {
        id: requestId,
        type: 'message',
        role: 'assistant',
        model,
        content: [],
        stop_reason: null,
        stop_sequence: null,
        usage: {
          input_tokens: 24,
          cache_creation_input_tokens: 0,
          cache_read_input_tokens: 0,
          output_tokens: 1,
        },
      },
    }],
    ['content_block_start', {
      type: 'content_block_start',
      index: 0,
      content_block: { type: 'text', text: '' },
    }],
    ['content_block_delta', {
      type: 'content_block_delta',
      index: 0,
      delta: { type: 'text_delta', text: 'capture-ok' },
    }],
    ['content_block_stop', { type: 'content_block_stop', index: 0 }],
    ['message_delta', {
      type: 'message_delta',
      delta: { stop_reason: 'end_turn', stop_sequence: null },
      usage: {
        input_tokens: 24,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        output_tokens: 2,
      },
    }],
    ['message_stop', { type: 'message_stop' }],
  ]
  return `${events.map(([event, value]) => `event: ${event}\ndata: ${JSON.stringify(value)}\n`).join('\n')}\n`
}

async function startFakeAnthropic(records) {
  for (;;) {
    const candidate = http.createServer((request, response) => {
      const chunks = []
      let totalBytes = 0
      request.on('data', (chunk) => {
        totalBytes += chunk.length
        if (totalBytes > MAX_CAPTURE_BYTES) {
          response.writeHead(413, { connection: 'close' })
          response.end()
          request.destroy()
          return
        }
        chunks.push(chunk)
      })
      request.on('end', () => {
        if (totalBytes > MAX_CAPTURE_BYTES) return
        const url = new URL(request.url || '/', 'http://127.0.0.1')
        if (request.method !== 'POST') {
          response.writeHead(405, { connection: 'close' })
          response.end()
          return
        }
        if (url.pathname.endsWith('/v1/messages/count_tokens')) {
          records.push({ caseId: activeCaseId, kind: 'count_tokens' })
          const responseBody = Buffer.from(JSON.stringify({ input_tokens: 24 }))
          response.writeHead(200, {
            'content-type': 'application/json',
            'content-length': responseBody.length,
            connection: 'close',
          })
          response.end(responseBody)
          return
        }
        if (!url.pathname.endsWith('/v1/messages')) {
          records.push({ caseId: activeCaseId, kind: 'unknown', path: url.pathname })
          response.writeHead(404, { connection: 'close' })
          response.end()
          return
        }
        const bodyBuffer = Buffer.concat(chunks)
        let facts
        try {
          facts = selectedRequestFacts(bodyBuffer, request)
        } catch {
          records.push({ caseId: activeCaseId, kind: 'invalid_json', bodySha256: sha256(bodyBuffer) })
          response.writeHead(400, { connection: 'close' })
          response.end()
          return
        }
        const requestId = `msg_${crypto.randomBytes(12).toString('hex')}`
        records.push({ caseId: activeCaseId, kind: 'messages', facts })
        const payload = ssePayload(String(facts.model || 'claude-opus-4-7'), requestId)
        response.writeHead(200, {
          'content-type': 'text/event-stream; charset=utf-8',
          'cache-control': 'no-cache',
          connection: 'close',
          'request-id': requestId,
        })
        response.end(payload)
      })
    })
    await new Promise((resolve, reject) => {
      candidate.once('error', reject)
      candidate.listen(0, '127.0.0.1', resolve)
    })
    const address = candidate.address()
    const selected = typeof address === 'object' && address ? address.port : 0
    if (selected !== 9022) {
      fakeServer = candidate
      fakePort = selected
      return
    }
    await new Promise((resolve) => candidate.close(resolve))
  }
}

function parseClaudeOutput(stdoutText) {
  const events = []
  for (const line of stdoutText.split(/\r?\n/)) {
    if (!line.trim()) continue
    try {
      events.push(JSON.parse(line))
    } catch {}
  }
  const result = events.findLast((event) => event?.type === 'result')
  const assistantText = events
    .filter((event) => event?.type === 'assistant')
    .flatMap((event) => event?.message?.content || [])
    .filter((block) => block?.type === 'text')
    .map((block) => block.text || '')
    .join('')
  return { result, assistantText }
}

async function runClaude({ effort, round, baseUrl }) {
  throwIfShutdownRequested()
  const caseId = `${effort}-${round}`
  const home = path.join(TEMP_ROOT, 'homes', caseId)
  const configDir = path.join(TEMP_ROOT, 'configs', caseId)
  const projectRoot = path.join(TEMP_ROOT, 'projects', caseId)
  fs.mkdirSync(home, { recursive: true, mode: 0o700 })
  fs.mkdirSync(configDir, { recursive: true, mode: 0o700 })
  fs.mkdirSync(projectRoot, { recursive: true, mode: 0o700 })
  const environment = validationChildEnvironment({
    HOME: home,
    CLAUDE_CONFIG_DIR: configDir,
    ANTHROPIC_BASE_URL: baseUrl,
    ANTHROPIC_API_KEY: FAKE_KEY,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1',
    DISABLE_AUTOUPDATER: '1',
    DISABLE_ERROR_REPORTING: '1',
    DISABLE_TELEMETRY: '1',
    CI: '1',
    TERM: 'dumb',
  })
  for (const name of [
    'CLAUDECODE',
    'ANTHROPIC_AUTH_TOKEN',
    'CLAUDE_CODE_USE_BEDROCK',
    'CLAUDE_CODE_USE_VERTEX',
    'CLAUDE_CODE_USE_FOUNDRY',
  ]) delete environment[name]

  const args = [
    '--bare',
    '--print',
    '--verbose',
    '--output-format',
    'stream-json',
    '--include-partial-messages',
    '--no-session-persistence',
    '--prompt-suggestions',
    'false',
    '--disable-slash-commands',
    '--session-id',
    crypto.randomUUID(),
    '--model',
    'opus',
    '--tools',
    '',
  ]
  if (effort !== 'absent') args.push('--effort', effort)
  args.push('--', `Reply with exactly capture-ok. Protocol capture ${caseId}.`)

  activeCaseId = caseId
  const started = performance.now()
  const child = spawn(CLAUDE, args, {
    cwd: projectRoot,
    env: environment,
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  ACTIVE_CHILDREN.add(child)
  const stdout = []
  const stderr = []
  child.stdout.on('data', (chunk) => stdout.push(chunk))
  child.stderr.on('data', (chunk) => stderr.push(chunk))
  const outcome = await new Promise((resolve) => {
    let settled = false
    const finish = (value) => {
      if (settled) return
      settled = true
      clearTimeout(timeout)
      resolve(value)
    }
    const timeout = setTimeout(
      () => finish({ code: null, signal: null, timedOut: true, spawnError: null }),
      60_000,
    )
    child.once('error', (error) => {
      finish({ code: null, signal: null, timedOut: false, spawnError: error })
    })
    child.once('exit', (code, signal) => {
      finish({ code, signal, timedOut: false, spawnError: null })
    })
  })
  if (outcome.spawnError) {
    ACTIVE_CHILDREN.delete(child)
    activeCaseId = null
    throw new Error(`${caseId}: failed to start Claude CLI: ${outcome.spawnError.message}`)
  }
  await stopChild(child)
  activeCaseId = null
  throwIfShutdownRequested()
  const stdoutText = Buffer.concat(stdout).toString('utf8')
  const stderrText = Buffer.concat(stderr).toString('utf8')
  return {
    caseId,
    durationMs: Math.round(performance.now() - started),
    ...outcome,
    stdoutText,
    stdoutSha256: sha256(stdoutText),
    stderrSha256: sha256(stderrText),
  }
}

function stableField(value) {
  return JSON.stringify(value)
}

async function main() {
  const mainStarted = performance.now()
  const records = []
  let report = null
  try {
    await startFakeAnthropic(records)
    assert.notEqual(fakePort, 9022)
    assert.deepEqual(listeningPids(fakePort), [process.pid])
    const baseUrl = `http://127.0.0.1:${fakePort}`
    const cases = []
    for (const effort of EFFORTS) {
      for (let round = 1; round <= ROUNDS; round += 1) {
        const cli = await runClaude({ effort, round, baseUrl })
        const parsed = parseClaudeOutput(cli.stdoutText)
        assert.equal(cli.timedOut, false, `${cli.caseId}: Claude CLI timed out`)
        assert.equal(cli.code, 0, `${cli.caseId}: Claude CLI exit status; stderr hash ${cli.stderrSha256}`)
        assert.equal(parsed.assistantText.includes('capture-ok'), true, `${cli.caseId}: fake response was not consumed`)
        const messageRecords = records.filter((record) => record.caseId === cli.caseId && record.kind === 'messages')
        assert.equal(messageRecords.length, 1, `${cli.caseId}: expected one inference request`)
        cases.push({
          id: cli.caseId,
          effort,
          round,
          durationMs: cli.durationMs,
          facts: messageRecords[0].facts,
          stdoutSha256: cli.stdoutSha256,
          stderrSha256: cli.stderrSha256,
        })
      }
    }

    const byEffort = Object.fromEntries(EFFORTS.map((effort) => {
      const effortCases = cases.filter((item) => item.effort === effort)
      const thinkingVariants = [...new Set(effortCases.map((item) => stableField(item.facts.thinking)))]
      const outputConfigVariants = [...new Set(effortCases.map((item) => stableField(item.facts.outputConfig)))]
      const models = [...new Set(effortCases.map((item) => item.facts.model))]
      const bodySizes = effortCases.map((item) => item.facts.bodyBytes)
      return [effort, {
        rounds: effortCases.length,
        thinkingVariants: thinkingVariants.map(JSON.parse),
        outputConfigVariants: outputConfigVariants.map(JSON.parse),
        models,
        bodyBytesMin: Math.min(...bodySizes),
        bodyBytesMax: Math.max(...bodySizes),
        uniqueBodyHashes: new Set(effortCases.map((item) => item.facts.bodySha256)).size,
        streamValues: [...new Set(effortCases.map((item) => item.facts.stream))],
        topLevelKeyVariants: [...new Set(effortCases.map((item) => stableField(item.facts.topLevelKeys)))].map(JSON.parse),
      }]
    }))

    report = {
      schemaVersion: 1,
      result: 'observation_complete',
      runId: RUN_ID,
      gitHead: commandOutput('git', ['rev-parse', 'HEAD']),
      claudeCliVersion: commandOutput(CLAUDE, ['--version']),
      rounds: ROUNDS,
      efforts: EFFORTS,
      totalCases: cases.length,
      totalMessageRequests: records.filter((record) => record.kind === 'messages').length,
      totalCountTokenRequests: records.filter((record) => record.kind === 'count_tokens').length,
      unknownRequests: records.filter((record) => record.kind === 'unknown'),
      invalidJsonRequests: records.filter((record) => record.kind === 'invalid_json').length,
      byEffort,
      cliDurationMsTotal: cases.reduce((sum, item) => sum + item.durationMs, 0),
      isolation: {
        fakePort,
        fakeKey: true,
        isolatedHomePerCase: true,
        isolatedClaudeConfigPerCase: true,
        isolatedProjectPerCase: true,
        forbiddenPorts: [9022],
        protected9022ProbeSkipped: true,
      },
    }
  } finally {
    const cleanup = await cleanupOwnedRuntime()
    if (report) {
      report.cleanup = {
        ...cleanup,
        protected9022ProbeSkipped: true,
      }
    }
  }

  assert.ok(report)
  report.wallDurationMs = Math.round(performance.now() - mainStarted)
  assert.equal(report.totalCases, EFFORTS.length * ROUNDS)
  assert.equal(report.totalMessageRequests, report.totalCases)
  assert.equal(report.invalidJsonRequests, 0)
  assert.deepEqual(report.unknownRequests, [])
  assert.equal(Object.values(report.cleanup).every(Boolean), true)
  const serialized = JSON.stringify(report, null, 2)
  assert.equal(serialized.includes(FAKE_KEY), false)
  assert.equal(serialized.includes(TEMP_ROOT), false)
  process.stdout.write(`${serialized}\n`)
}

for (const [signal, exitCode] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  process.once(signal, () => {
    if (shutdownExitCode !== null) return
    shutdownExitCode = exitCode
    cleanupOwnedRuntime()
      .catch(() => {})
      .finally(() => process.exit(exitCode))
  })
}

main().catch(async (error) => {
  await cleanupOwnedRuntime().catch(() => {})
  if (shutdownExitCode !== null && error instanceof ShutdownRequested) return
  process.stderr.write(`Claude CLI thinking/effort capture failed: ${error.message}\n`)
  process.exitCode = 1
})
