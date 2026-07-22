#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const RUNNER = path.join(ROOT, 'feature/tests/bare-invoke-claude-cli.mjs')
const CASES = [
  ['SIGHUP', 129],
  ['SIGINT', 130],
  ['SIGTERM', 143],
]

function pidAlive(pid) {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

async function portAcceptsConnections(port) {
  return await new Promise((resolve) => {
    const socket = net.connect({ host: '127.0.0.1', port })
    socket.setTimeout(300)
    socket.once('connect', () => {
      socket.destroy()
      resolve(true)
    })
    socket.once('timeout', () => {
      socket.destroy()
      resolve(false)
    })
    socket.once('error', () => resolve(false))
  })
}

async function waitForReady(child, timeoutMs = 10_000) {
  return await new Promise((resolve, reject) => {
    let buffer = ''
    const timer = setTimeout(() => reject(new Error('signal fixture readiness timed out')), timeoutMs)
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', (chunk) => {
      buffer += chunk
      const newline = buffer.indexOf('\n')
      if (newline < 0) return
      clearTimeout(timer)
      try {
        resolve(JSON.parse(buffer.slice(0, newline)))
      } catch (error) {
        reject(error)
      }
    })
    child.once('error', (error) => {
      clearTimeout(timer)
      reject(error)
    })
    child.once('exit', (code, signal) => {
      clearTimeout(timer)
      reject(new Error(`signal fixture exited before ready: code=${code} signal=${signal}`))
    })
  })
}

async function waitForExit(child, timeoutMs = 20_000) {
  return await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('signal fixture exit timed out')), timeoutMs)
    child.once('exit', (code, signal) => {
      clearTimeout(timer)
      resolve({ code, signal })
    })
  })
}

for (const [signal, expectedCode] of CASES) {
  test(`${signal} cleans owned process group, ports, Redis prefix and temp root`, async () => {
    const parentRoot = fs.mkdtempSync(path.join(os.tmpdir(), `bare-invoke-signal-${signal}-`))
    const ackPath = path.join(parentRoot, 'redis-cleanup-ack')
    const child = spawn(process.execPath, [RUNNER], {
      cwd: ROOT,
      env: {
        ...process.env,
        KIRO_BARE_INVOKE_SIGNAL_FIXTURE: '1',
        KIRO_SIGNAL_ACK_PATH: ackPath,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    const stderr = []
    child.stderr.on('data', (chunk) => stderr.push(chunk))

    try {
      const ready = await waitForReady(child)
      assert.equal(ready.ready, true)
      assert.equal(await portAcceptsConnections(ready.fakePort), true)
      assert.equal(await portAcceptsConnections(ready.redisPort), true)
      assert.equal(pidAlive(ready.ownedChildPid), true)
      assert.equal(fs.existsSync(ready.tempRoot), true)

      const exiting = waitForExit(child)
      child.kill(signal)
      const exit = await exiting
      assert.deepEqual(exit, { code: expectedCode, signal: null })
      assert.equal(pidAlive(ready.ownedChildPid), false)
      assert.equal(fs.existsSync(ready.tempRoot), false)
      assert.equal(fs.existsSync(ackPath), true)
      assert.equal(
        fs.readFileSync(ackPath, 'utf8'),
        'bounded-owned-prefix-cleanup\n',
      )
      assert.equal(await portAcceptsConnections(ready.fakePort), false)
      assert.equal(await portAcceptsConnections(ready.redisPort), false)
      assert.equal(Buffer.concat(stderr).toString('utf8'), '')
    } finally {
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
      fs.rmSync(parentRoot, { recursive: true, force: true })
    }
  })
}
