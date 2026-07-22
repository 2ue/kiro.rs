#!/usr/bin/env node

import assert from 'node:assert/strict'
import http from 'node:http'
import net from 'node:net'

function integerArgument(name, fallback, minimum, maximum) {
  const index = process.argv.indexOf(`--${name}`)
  const raw = index >= 0 ? process.argv[index + 1] : undefined
  const value = raw === undefined ? fallback : Number.parseInt(raw, 10)
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(`--${name} must be an integer in ${minimum}..${maximum}`)
  }
  return value
}

function stringArgument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`)
  const value = index >= 0 ? process.argv[index + 1] : fallback
  if (!value || value.startsWith('--')) throw new Error(`--${name} requires a value`)
  return value
}

const listenHost = stringArgument('listen-host', '127.0.0.1')
const listenPort = integerArgument('listen-port', 0, 0, 65535)
const apiHost = stringArgument('api-host', '127.0.0.1')
const apiPort = integerArgument('api-port', 0, 0, 65535)
const upstreamHost = stringArgument('upstream-host', '127.0.0.1')
const upstreamPort = integerArgument('upstream-port', 6379, 1, 65535)
const upstreamDatabase = integerArgument('database', 0, 0, 15)
const proxyName = stringArgument('name', 'redis')
const allowFlush = process.argv.includes('--allow-flush')

for (const port of [listenPort, apiPort, upstreamPort]) {
  if (port === 9022) throw new Error('port 9022 is protected')
}
if (!['127.0.0.1', 'localhost', '::1'].includes(upstreamHost)) {
  throw new Error('redis chaos proxy upstream must be loopback')
}

const state = { enabled: true, latencyMs: 0 }
const connections = new Set()
let shuttingDown = false
let resolveLifetime
const lifetime = new Promise((resolve) => { resolveLifetime = resolve })

function destroyPair(pair) {
  connections.delete(pair)
  pair.client.destroy()
  pair.upstream.destroy()
}

function delayedWriter(socket) {
  let chain = Promise.resolve()
  return (chunk) => {
    const copy = Buffer.from(chunk)
    const delay = state.latencyMs
    chain = chain.then(async () => {
      if (delay > 0) await new Promise((resolve) => setTimeout(resolve, delay))
      if (!socket.destroyed && state.enabled) socket.write(copy)
    }).catch(() => {})
  }
}

const proxyServer = net.createServer((client) => {
  client.setNoDelay(true)
  if (!state.enabled) {
    client.destroy()
    return
  }
  const upstream = net.connect({ host: upstreamHost, port: upstreamPort })
  upstream.setNoDelay(true)
  const pair = { client, upstream }
  connections.add(pair)
  const writeDownstream = delayedWriter(client)
  client.on('data', (chunk) => {
    if (!upstream.destroyed && state.enabled) upstream.write(chunk)
  })
  upstream.on('data', writeDownstream)
  client.on('error', () => destroyPair(pair))
  upstream.on('error', () => destroyPair(pair))
  client.on('close', () => destroyPair(pair))
  upstream.on('close', () => destroyPair(pair))
})

async function readJson(request) {
  const chunks = []
  let bytes = 0
  for await (const chunk of request) {
    bytes += chunk.length
    if (bytes > 64 * 1024) throw new Error('control request body is too large')
    chunks.push(chunk)
  }
  if (bytes === 0) return {}
  return JSON.parse(Buffer.concat(chunks).toString('utf8'))
}

function writeJson(response, status, body = {}) {
  const encoded = Buffer.from(JSON.stringify(body))
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': encoded.length,
    connection: 'close',
  })
  response.end(encoded)
}

function redisControlCommand(command) {
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host: upstreamHost, port: upstreamPort })
    const commands = []
    if (upstreamDatabase !== 0) commands.push(['SELECT', String(upstreamDatabase)])
    commands.push(command)
    const payload = Buffer.concat(commands.map((parts) => {
      const encoded = [Buffer.from(`*${parts.length}\r\n`)]
      for (const part of parts) {
        const bytes = Buffer.from(part)
        encoded.push(Buffer.from(`$${bytes.length}\r\n`), bytes, Buffer.from('\r\n'))
      }
      return Buffer.concat(encoded)
    }))
    const replies = []
    let pending = ''
    socket.setTimeout(5_000)
    socket.once('connect', () => socket.write(payload))
    socket.on('data', (chunk) => {
      pending += chunk.toString('utf8')
      for (;;) {
        const end = pending.indexOf('\r\n')
        if (end < 0) return
        const reply = pending.slice(0, end)
        pending = pending.slice(end + 2)
        if (reply.startsWith('-')) {
          socket.destroy()
          reject(new Error(`Redis control command failed: ${reply.slice(1)}`))
          return
        }
        if (!reply.startsWith('+') && !reply.startsWith(':')) {
          socket.destroy()
          reject(new Error('Redis control command returned an unsupported response'))
          return
        }
        replies.push(reply.startsWith(':') ? Number(reply.slice(1)) : reply.slice(1))
        if (replies.length === commands.length) {
          socket.end()
          resolve(replies.at(-1))
          return
        }
      }
    })
    socket.once('timeout', () => {
      socket.destroy()
      reject(new Error('Redis control command timed out'))
    })
    socket.once('error', reject)
  })
}

const apiServer = http.createServer(async (request, response) => {
  try {
    const url = new URL(request.url || '/', 'http://127.0.0.1')
    const proxyPath = `/proxies/${encodeURIComponent(proxyName)}`
    if (request.method === 'GET' && url.pathname === '/state') {
      writeJson(response, 200, {
        enabled: state.enabled,
        latencyMs: state.latencyMs,
        activeConnections: connections.size,
      })
      return
    }
    if (request.method === 'GET' && url.pathname === '/database/size') {
      const size = await redisControlCommand(['DBSIZE'])
      writeJson(response, 200, { database: upstreamDatabase, size })
      return
    }
    if (request.method === 'POST' && url.pathname === '/database/flush') {
      if (!allowFlush || upstreamDatabase === 0) {
        writeJson(response, 403, { error: 'flush requires --allow-flush and a nonzero database' })
        return
      }
      const body = await readJson(request)
      if (body.confirm !== 'isolated') {
        writeJson(response, 409, { error: 'flush requires isolated confirmation' })
        return
      }
      const result = await redisControlCommand(['FLUSHDB'])
      writeJson(response, 200, { database: upstreamDatabase, result })
      return
    }
    if (request.method === 'DELETE' && url.pathname.startsWith(`${proxyPath}/toxics/`)) {
      state.latencyMs = 0
      writeJson(response, 204)
      return
    }
    if (request.method === 'POST' && url.pathname === `${proxyPath}/toxics`) {
      const body = await readJson(request)
      assert.equal(body.type, 'latency')
      assert.equal(body.stream, 'downstream')
      const latency = Number(body?.attributes?.latency)
      if (!Number.isSafeInteger(latency) || latency < 0 || latency > 60_000) {
        throw new Error('latency toxic is outside 0..60000ms')
      }
      state.latencyMs = latency
      writeJson(response, 200, { name: body.name, type: body.type })
      return
    }
    if (request.method === 'POST' && url.pathname === proxyPath) {
      const body = await readJson(request)
      if (typeof body.enabled !== 'boolean') throw new Error('enabled must be boolean')
      state.enabled = body.enabled
      if (!state.enabled) {
        for (const pair of [...connections]) destroyPair(pair)
      }
      writeJson(response, 200, { name: proxyName, enabled: state.enabled })
      return
    }
    writeJson(response, 404, { error: 'unsupported chaos control endpoint' })
  } catch (error) {
    writeJson(response, 400, { error: String(error?.message || error) })
  }
})

async function closeServer(server) {
  if (!server.listening) return
  await new Promise((resolve) => server.close(resolve))
}

async function shutdown(exitCode = 0) {
  if (shuttingDown) return
  shuttingDown = true
  for (const pair of [...connections]) destroyPair(pair)
  await Promise.all([closeServer(proxyServer), closeServer(apiServer)])
  process.exitCode = exitCode
  resolveLifetime()
}

try {
  await new Promise((resolve, reject) => {
    proxyServer.once('error', reject)
    proxyServer.listen(listenPort, listenHost, resolve)
  })
  await new Promise((resolve, reject) => {
    apiServer.once('error', reject)
    apiServer.listen(apiPort, apiHost, resolve)
  })
} catch (error) {
  await shutdown(1)
  throw error
}

for (const signal of ['SIGHUP', 'SIGINT', 'SIGTERM']) {
  process.on(signal, () => { void shutdown(signal === 'SIGHUP' ? 129 : signal === 'SIGINT' ? 130 : 143) })
}

const proxyAddress = proxyServer.address()
const apiAddress = apiServer.address()
assert.equal(typeof proxyAddress, 'object')
assert.equal(typeof apiAddress, 'object')
process.stdout.write(`${JSON.stringify({
  ready: true,
  name: proxyName,
  proxyHost: listenHost,
  proxyPort: proxyAddress.port,
  apiHost,
  apiPort: apiAddress.port,
  upstreamHost,
  upstreamPort,
  upstreamDatabase,
  flushEnabled: allowFlush && upstreamDatabase !== 0,
  protected9022ProbeSkipped: true,
})}\n`)

await lifetime
