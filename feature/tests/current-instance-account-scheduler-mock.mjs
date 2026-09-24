#!/usr/bin/env node

import assert from 'node:assert/strict'
import http from 'node:http'
import crypto from 'node:crypto'

const BASE_URL = process.env.ACCOUNT_RUNTIME_CURRENT_BASE_URL || 'http://127.0.0.1:9022'
const ADMIN_KEY = process.env.ACCOUNT_RUNTIME_CURRENT_ADMIN_KEY || 'admin123'
const TEST_MODEL = process.env.ACCOUNT_RUNTIME_CURRENT_TEST_MODEL || 'claude-sonnet-4-20250514'
const REQUEST_PATH = process.env.ACCOUNT_RUNTIME_CURRENT_REQUEST_PATH || '/v1/messages'
const CC_REQUEST_PATH = process.env.ACCOUNT_RUNTIME_CURRENT_CC_REQUEST_PATH || '/cc/v1/messages'
const MOCK_PORTS = {
  primary: Number(process.env.ACCOUNT_RUNTIME_CURRENT_PRIMARY_PORT || 19181),
  secondary: Number(process.env.ACCOUNT_RUNTIME_CURRENT_SECONDARY_PORT || 19182),
  tertiary: Number(process.env.ACCOUNT_RUNTIME_CURRENT_TERTIARY_PORT || 19183),
}

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

function makeJsonResponse(status, payload) {
  return {
    status,
    headers: { 'content-type': 'application/json; charset=utf-8' },
    body: JSON.stringify(payload),
  }
}

function streamFrame(event, payload) {
  return `event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`
}

function successStreamFrames({ thinking = false, model = 'fake-sonnet', text = 'fake response' } = {}) {
  const frames = [
    streamFrame('message_start', {
      type: 'message_start',
      message: {
        id: `msg_${crypto.randomUUID().replace(/-/g, '').slice(0, 12)}`,
        type: 'message',
        role: 'assistant',
        content: [],
        model,
        stop_reason: null,
        usage: { input_tokens: 10, output_tokens: 0 },
      },
    }),
    streamFrame('content_block_start', {
      type: 'content_block_start',
      index: 0,
      content_block: { type: 'text', text: '' },
    }),
  ]
  if (thinking) {
    frames.push(
      streamFrame('content_block_delta', {
        type: 'content_block_delta',
        index: 0,
        delta: { type: 'thinking_delta', thinking: 'considering' },
      })
    )
  }
  frames.push(
    streamFrame('content_block_delta', {
      type: 'content_block_delta',
      index: 0,
      delta: { type: 'text_delta', text },
    }),
    streamFrame('content_block_stop', {
      type: 'content_block_stop',
      index: 0,
    }),
    streamFrame('message_delta', {
      type: 'message_delta',
      delta: { stop_reason: 'end_turn' },
      usage: { output_tokens: 3 },
    }),
    streamFrame('message_stop', { type: 'message_stop' })
  )
  return frames
}

function toolUseStreamFrames({ model = 'fake-sonnet', name = 'Bash' } = {}) {
  return [
    streamFrame('message_start', {
      type: 'message_start',
      message: {
        id: `msg_${crypto.randomUUID().replace(/-/g, '').slice(0, 12)}`,
        type: 'message',
        role: 'assistant',
        content: [],
        model,
        stop_reason: null,
        usage: { input_tokens: 10, output_tokens: 0 },
      },
    }),
    streamFrame('content_block_start', {
      type: 'content_block_start',
      index: 0,
      content_block: { type: 'tool_use', id: 'toolu_fake_1', name, input: {} },
    }),
    streamFrame('content_block_delta', {
      type: 'content_block_delta',
      index: 0,
      delta: { type: 'input_json_delta', partial_json: '{"command":"echo ok"}' },
    }),
    streamFrame('content_block_stop', {
      type: 'content_block_stop',
      index: 0,
    }),
    streamFrame('message_delta', {
      type: 'message_delta',
      delta: { stop_reason: 'tool_use' },
      usage: { output_tokens: 6 },
    }),
    streamFrame('message_stop', { type: 'message_stop' }),
  ]
}

function createMockServer(name, port, initialMode = { type: 'success' }) {
  const state = {
    name,
    mode: initialMode,
    hits: 0,
    successHits: 0,
    errorHits: 0,
    requests: [],
  }

  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url || '/', `http://${req.headers.host || '127.0.0.1'}`)
    if (req.method === 'POST' && url.pathname === '/__control') {
      const body = await readJson(req)
      if (body?.mode) state.mode = body.mode
      res.writeHead(200, { 'content-type': 'application/json; charset=utf-8' })
      res.end(JSON.stringify({ ok: true, mode: state.mode }))
      return
    }
    if (req.method === 'GET' && url.pathname === '/__state') {
      res.writeHead(200, { 'content-type': 'application/json; charset=utf-8' })
      res.end(JSON.stringify(state))
      return
    }
    if (req.method !== 'POST' || (url.pathname !== '/v1/messages' && url.pathname !== '/v1/messages/count_tokens')) {
      res.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' })
      res.end('not found')
      return
    }

    const body = await readJson(req)
    state.hits += 1
    state.requests.push({ path: url.pathname, headers: req.headers, body })

    const mode = state.mode || { type: 'success' }
    if (mode.type === 'rate_limit_429') {
      state.errorHits += 1
      const payload = makeJsonResponse(429, {
        type: 'error',
        error: { type: 'rate_limit_error', message: mode.message || `${name} rate limited` },
      })
      res.writeHead(payload.status, payload.headers)
      res.end(payload.body)
      return
    }
    if (mode.type === 'server_error_500') {
      state.errorHits += 1
      const payload = makeJsonResponse(500, {
        type: 'error',
        error: { type: 'api_error', message: mode.message || `${name} server error` },
      })
      res.writeHead(payload.status, payload.headers)
      res.end(payload.body)
      return
    }

    const stream = body?.stream !== false && url.pathname === '/v1/messages'
    const bodyText = JSON.stringify(body ?? {})
    const text = mode.text || `${name} ok ${state.hits}`
    const delayMs = Number(mode.delayMs || 0)

    if (delayMs > 0) {
      await delay(delayMs)
    }

    if (url.pathname === '/v1/messages/count_tokens') {
      state.successHits += 1
      const payload = makeJsonResponse(200, {
        input_tokens: Math.max(1, Math.ceil(bodyText.length / 8)),
        output_tokens: 0,
      })
      res.writeHead(payload.status, payload.headers)
      res.end(payload.body)
      return
    }

    if (mode.type === 'malformed_sse') {
      state.errorHits += 1
      res.writeHead(200, {
        'content-type': 'text/event-stream; charset=utf-8',
        connection: 'keep-alive',
        'cache-control': 'no-cache',
      })
      res.write('event: message_start\ndata: {"type":"message_start"}\n\n')
      res.write('event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"broken"\n\n')
      res.end()
      return
    }

    if (!stream || mode.type === 'success_json') {
      state.successHits += 1
      const payload = makeJsonResponse(200, {
        id: `msg_${name}_${state.hits}`,
        type: 'message',
        role: 'assistant',
        content: [{ type: 'text', text }],
        model: mode.model || 'fake-sonnet',
        stop_reason: 'end_turn',
        usage: { input_tokens: 10, output_tokens: 3 },
      })
      res.writeHead(payload.status, payload.headers)
      res.end(payload.body)
      return
    }

    state.successHits += 1
    res.writeHead(200, {
      'content-type': 'text/event-stream; charset=utf-8',
      connection: 'keep-alive',
      'cache-control': 'no-cache',
      'x-accel-buffering': 'no',
    })
    for (const frame of mode.type === 'tool_use_stream'
      ? toolUseStreamFrames({ model: mode.model || 'fake-sonnet', name: mode.toolName || 'Bash' })
      : successStreamFrames({
          thinking: mode.type === 'thinking_stream',
          model: mode.model || 'fake-sonnet',
          text,
        })) {
      res.write(frame)
      if (mode.chunkDelayMs) await delay(mode.chunkDelayMs)
    }
    res.end()
  })

  server.listen(port, '127.0.0.1')

  return {
    name,
    port,
    state,
    server,
    async close() {
      await new Promise((resolve) => server.close(resolve))
    },
    setMode(mode) {
      state.mode = mode
    },
  }
}

async function readJson(req) {
  const chunks = []
  for await (const chunk of req) chunks.push(Buffer.from(chunk))
  const text = Buffer.concat(chunks).toString('utf8')
  if (!text.trim()) return {}
  return JSON.parse(text)
}

async function admin(path, options = {}) {
  const headers = new Headers(options.headers || {})
  headers.set('x-api-key', ADMIN_KEY)
  if (options.body && !headers.has('content-type')) headers.set('content-type', 'application/json')
  const res = await fetch(`${BASE_URL}${path}`, {
    method: options.method || 'GET',
    headers,
    body: options.body ? JSON.stringify(options.body) : undefined,
  })
  const text = await res.text()
  return { status: res.status, text, json: tryParseJson(text) }
}

function tryParseJson(text) {
  try {
    return text ? JSON.parse(text) : null
  } catch {
    return null
  }
}

async function ensureRequestKey() {
  const response = await admin('/api/admin/security/request-keys', { method: 'POST', body: {} })
  assert.equal(response.status, 200, `request key creation failed: ${response.text}`)
  const keys = response.json?.requestApiKeys || []
  const created = [...keys].reverse().find((item) => !item.primary) || keys[0]
  assert.ok(created?.apiKey, 'request key response missing apiKey')
  return created
}

async function deleteRequestKey(id) {
  const response = await admin(`/api/admin/security/request-keys/${id}`, { method: 'DELETE' })
  assert.equal(response.status, 200, `request key delete failed: ${response.text}`)
}

async function listAccounts() {
  const response = await admin('/api/admin/accounts')
  assert.equal(response.status, 200, `list accounts failed: ${response.text}`)
  return response.json.accounts || []
}

async function getAccountsStatus() {
  const response = await admin('/api/admin/accounts/status')
  return response
}

async function setLoadBalancingMode(mode) {
  const response = await admin('/api/admin/config/load-balancing', {
    method: 'PUT',
    body: { mode },
  })
  assert.equal(response.status, 200, `set load balancing failed: ${response.text}`)
  return response.json
}

async function getLoadBalancingMode() {
  const response = await admin('/api/admin/config/load-balancing')
  assert.equal(response.status, 200, `get load balancing failed: ${response.text}`)
  return response.json?.mode
}

async function getRuntimeConfig() {
  const response = await admin('/api/admin/config/runtime')
  assert.equal(response.status, 200, `get runtime config failed: ${response.text}`)
  return response.json
}

async function updateRuntimeConfig(next) {
  const response = await admin('/api/admin/config/runtime', {
    method: 'PUT',
    body: next,
  })
  assert.equal(response.status, 200, `update runtime config failed: ${response.text}`)
  return response.json
}

async function waitForDispatchableAccounts(expectedCount, timeoutMs = 60000) {
  const deadline = Date.now() + timeoutMs
  let lastStatus = []
  let lastError = null
  while (Date.now() < deadline) {
    const response = await getAccountsStatus()
    if (response.status === 200) {
      lastStatus = response.json.accounts || []
      const dispatchableCount = lastStatus.filter((item) => item.dispatchable).length
      if (dispatchableCount >= expectedCount) {
        await delay(2000)
        return lastStatus
      }
      lastError = null
    } else if (response.status >= 500) {
      lastError = response.text
    } else {
      throw new Error(`unexpected accounts status ${response.status}: ${response.text}`)
    }
    await delay(250)
  }
  const suffix = lastError ? ` lastError=${lastError}` : ''
  throw new Error(
    `timed out waiting for ${expectedCount} dispatchable accounts: ${JSON.stringify(lastStatus)}${suffix}`
  )
}

async function createAccount(request) {
  const response = await admin('/api/admin/accounts', { method: 'POST', body: request })
  assert.equal(response.status, 200, `create account failed: ${response.text}`)
  return response.json
}

async function updateAccount(id, request) {
  const response = await admin(`/api/admin/accounts/${id}`, { method: 'PUT', body: request })
  assert.equal(response.status, 200, `update account ${id} failed: ${response.text}`)
  return response.json
}

async function setAccountEnabled(id, enabled) {
  const response = await admin(`/api/admin/accounts/${id}/enabled`, {
    method: 'POST',
    body: { enabled },
  })
  assert.equal(response.status, 200, `set account ${id} enabled=${enabled} failed: ${response.text}`)
  return response.json
}

async function deleteAccount(id) {
  const response = await admin(`/api/admin/accounts/${id}`, { method: 'DELETE' })
  assert.equal(response.status, 200, `delete account ${id} failed: ${response.text}`)
}

async function sendMessage(requestKey, {
  path = REQUEST_PATH,
  stream = true,
  model = TEST_MODEL,
  prompt = 'hello',
  timeoutMs = 20000,
  extra = {},
} = {}) {
  const body = {
    model,
    max_tokens: 64,
    stream,
    messages: [{ role: 'user', content: prompt }],
    ...extra,
  }
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(new Error(`timeout after ${timeoutMs}ms`)), timeoutMs)
  try {
    const res = await fetch(`${BASE_URL}${path}`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'x-api-key': requestKey,
      },
      body: JSON.stringify(body),
      signal: controller.signal,
    })
    const text = await res.text()
    return { status: res.status, text, json: tryParseJson(text), body }
  } finally {
    clearTimeout(timer)
  }
}

async function runBatch(requestKey, {
  name,
  count,
  concurrency,
  path = REQUEST_PATH,
  stream = true,
  promptPrefix = name,
  extra = {},
  interRequestDelayMs = 0,
}) {
  const results = []
  let next = 0
  const workers = Array.from({ length: Math.max(1, concurrency) }, async () => {
    while (true) {
      const index = next
      if (index >= count) return
      next += 1
      const prompt = `${promptPrefix}-${index}`
      const result = await sendMessage(requestKey, { path, stream, prompt, extra })
      results[index] = result
      if (interRequestDelayMs > 0) {
        await delay(interRequestDelayMs)
      }
    }
  })
  await Promise.all(workers)
  return results
}

function summarizeBatch(results) {
  const statusCounts = new Map()
  for (const result of results) {
    const key = String(result.status)
    statusCounts.set(key, (statusCounts.get(key) || 0) + 1)
  }
  return {
    total: results.length,
    statusCounts: Object.fromEntries(statusCounts),
    success: results.filter((result) => result.status >= 200 && result.status < 300).length,
    failures: results.filter((result) => result.status >= 400).length,
  }
}

async function getUsageRecords(requestApiKeyId, endpoint) {
  const url = new URL(`${BASE_URL}/api/admin/usage-records`)
  if (requestApiKeyId) url.searchParams.set('requestApiKeyId', requestApiKeyId)
  if (endpoint) url.searchParams.set('endpoint', endpoint)
  const response = await admin(`${url.pathname}${url.search}`)
  assert.equal(response.status, 200, `usage records query failed: ${response.text}`)
  return response.json || { total: 0, records: [] }
}

function createAssertions(label, results, upstreams, expected) {
  const summary = summarizeBatch(results)
  const hits = Object.fromEntries(Object.entries(upstreams).map(([key, upstream]) => [key, upstream.state.hits]))
  console.log(JSON.stringify({ label, summary, hits }, null, 2))
  if (expected.successMin !== undefined) {
    assert.ok(summary.success >= expected.successMin, `${label}: success ${summary.success} < ${expected.successMin}`)
  }
  if (expected.failMax !== undefined) {
    assert.ok(summary.failures <= expected.failMax, `${label}: failures ${summary.failures} > ${expected.failMax}`)
  }
  if (expected.primaryHitsMin !== undefined) {
    assert.ok(hits.primary >= expected.primaryHitsMin, `${label}: primary hits ${hits.primary} < ${expected.primaryHitsMin}`)
  }
  if (expected.secondaryHitsMin !== undefined) {
    assert.ok(hits.secondary >= expected.secondaryHitsMin, `${label}: secondary hits ${hits.secondary} < ${expected.secondaryHitsMin}`)
  }
  if (expected.tertiaryHitsMin !== undefined) {
    assert.ok(hits.tertiary >= expected.tertiaryHitsMin, `${label}: tertiary hits ${hits.tertiary} < ${expected.tertiaryHitsMin}`)
  }
  if (expected.primaryShareMin !== undefined) {
    assert.ok(
      hits.primary >= expected.primaryShareMin,
      `${label}: primary share too small, hits=${hits.primary} summary=${JSON.stringify(summary)}`
    )
  }
}

async function main() {
  const upstreams = {
    primary: createMockServer('primary', MOCK_PORTS.primary, { type: 'success', delayMs: 80, text: 'primary ok' }),
    secondary: createMockServer('secondary', MOCK_PORTS.secondary, { type: 'success', delayMs: 20, text: 'secondary ok' }),
    tertiary: createMockServer('tertiary', MOCK_PORTS.tertiary, { type: 'success', delayMs: 20, text: 'tertiary ok' }),
  }

  const createdAccounts = []
  const disabledAccounts = []
  const createdRequestKeys = []
  const originalLoadBalancingMode = await getLoadBalancingMode()
  const runtimeConfig = await getRuntimeConfig()
  const originalExternalPools = structuredClone(runtimeConfig.accountRuntime)

  try {
    const requestKeyResponse = await ensureRequestKey()
    createdRequestKeys.push(requestKeyResponse)
    const requestKey = requestKeyResponse.apiKey
    const requestKeyId = requestKeyResponse.id
    assert.ok(requestKey, 'no usable request key returned')

    const accounts = await listAccounts()
    for (const account of accounts) {
      if (account.enabled) {
        disabledAccounts.push(account)
        await setAccountEnabled(account.id, false)
      }
    }

    await setLoadBalancingMode('priority')
    await updateRuntimeConfig({
      ...runtimeConfig,
      accountRuntime: {
        ...originalExternalPools,
        externalPoolsEnabled: true,
        externalDirectPolicyEnabled: true,
        fallbackOnNoAvailableCredentials: true,
        externalPoolLocalRescueEnabled: true,
        externalPoolAutoDisableEnabled: false,
        externalPoolRetryMaxAttempts: 4,
        externalPoolSamePoolRetryCount: 1,
        externalPoolSamePoolRetryDelayMs: 20,
        externalPoolRateLimitCooldownSecs: 1,
        externalPoolServerErrorCooldownSecs: 1,
        externalPoolNetworkErrorCooldownSecs: 1,
        externalPoolProtocolErrorCooldownSecs: 1,
        externalPoolModelUnavailableCooldownSecs: 1,
        externalPoolRequestTimeoutSecs: 20,
        externalPoolStreamIdleTimeoutSecs: 20,
      },
      externalPools: {
        ...originalExternalPools,
        externalPoolsEnabled: true,
        externalDirectPolicyEnabled: true,
        fallbackOnNoAvailableCredentials: true,
        externalPoolLocalRescueEnabled: true,
        externalPoolAutoDisableEnabled: false,
        externalPoolRetryMaxAttempts: 4,
        externalPoolSamePoolRetryCount: 1,
        externalPoolSamePoolRetryDelayMs: 20,
        externalPoolRateLimitCooldownSecs: 1,
        externalPoolServerErrorCooldownSecs: 1,
        externalPoolNetworkErrorCooldownSecs: 1,
        externalPoolProtocolErrorCooldownSecs: 1,
        externalPoolModelUnavailableCooldownSecs: 1,
        externalPoolRequestTimeoutSecs: 20,
        externalPoolStreamIdleTimeoutSecs: 20,
      },
    })

    const accountPayloads = [
      {
        name: `mock-priority-${Date.now()}`,
        baseUrl: `http://127.0.0.1:${MOCK_PORTS.primary}`,
        apiKey: 'mock-primary-key',
        authType: 'x_api_key',
        enabled: true,
        priority: 1,
        maxConcurrentRequests: 4,
        usageProjectionMode: 'current_path_policy',
        requestBodyMode: 'raw_passthrough',
        rawModelMode: 'none',
        autoDisablePolicy: 'disabled',
        preOutputStreamRetryMode: 'enabled',
        modelMappingMode: 'passthrough',
        modelMappingRequireMatch: false,
        supportedModels: [TEST_MODEL],
        routeMode: 'allow_all',
        routeRules: [],
        preservePath: true,
        normalizeModelVersionDots: false,
        notes: 'current-instance-mock-primary',
      },
      {
        name: `mock-secondary-${Date.now()}`,
        baseUrl: `http://127.0.0.1:${MOCK_PORTS.secondary}`,
        apiKey: 'mock-secondary-key',
        authType: 'x_api_key',
        enabled: true,
        priority: 10,
        maxConcurrentRequests: 4,
        usageProjectionMode: 'current_path_policy',
        requestBodyMode: 'raw_passthrough',
        rawModelMode: 'none',
        autoDisablePolicy: 'disabled',
        preOutputStreamRetryMode: 'enabled',
        modelMappingMode: 'passthrough',
        modelMappingRequireMatch: false,
        supportedModels: [TEST_MODEL],
        routeMode: 'allow_all',
        routeRules: [],
        preservePath: true,
        normalizeModelVersionDots: false,
        notes: 'current-instance-mock-secondary',
      },
      {
        name: `mock-tertiary-${Date.now()}`,
        baseUrl: `http://127.0.0.1:${MOCK_PORTS.tertiary}`,
        apiKey: 'mock-tertiary-key',
        authType: 'x_api_key',
        enabled: true,
        priority: 20,
        maxConcurrentRequests: 4,
        usageProjectionMode: 'current_path_policy',
        requestBodyMode: 'raw_passthrough',
        rawModelMode: 'none',
        autoDisablePolicy: 'disabled',
        preOutputStreamRetryMode: 'enabled',
        modelMappingMode: 'passthrough',
        modelMappingRequireMatch: false,
        supportedModels: [TEST_MODEL],
        routeMode: 'allow_all',
        routeRules: [],
        preservePath: true,
        normalizeModelVersionDots: false,
        notes: 'current-instance-mock-tertiary',
      },
    ]

    for (const payload of accountPayloads) {
      const account = await createAccount(payload)
      createdAccounts.push(account)
    }

    assert.ok(createdAccounts.length === 3, 'expected three mock accounts')
    await waitForDispatchableAccounts(createdAccounts.length)

    const byName = Object.fromEntries(createdAccounts.map((account) => [account.name, account]))
    const primary = byName[accountPayloads[0].name]
    const secondary = byName[accountPayloads[1].name]
    const tertiary = byName[accountPayloads[2].name]
    assert.ok(primary && secondary && tertiary, 'missing created mock accounts')

    const normalStream = await runBatch(requestKey, {
      name: 'normal-stream',
      count: 12,
      concurrency: 2,
      path: REQUEST_PATH,
      stream: true,
      promptPrefix: 'normal',
    })
    createAssertions('normal-stream', normalStream, upstreams, {
      successMin: 12,
      failMax: 0,
      primaryHitsMin: 8,
      secondaryHitsMin: 0,
      tertiaryHitsMin: 0,
    })

    upstreams.primary.setMode({ type: 'success', delayMs: 350, text: 'primary slow' })
    upstreams.secondary.setMode({ type: 'success', delayMs: 20, text: 'secondary spill' })
    upstreams.tertiary.setMode({ type: 'success', delayMs: 20, text: 'tertiary spill' })
    const burstStream = await runBatch(requestKey, {
      name: 'burst-stream',
      count: 24,
      concurrency: 8,
      path: REQUEST_PATH,
      stream: true,
      promptPrefix: 'burst',
    })
    createAssertions('burst-stream', burstStream, upstreams, {
      successMin: 23,
      failMax: 1,
      primaryHitsMin: 1,
      secondaryHitsMin: 1,
    })

    await setLoadBalancingMode('weighted_least_inflight')
    upstreams.primary.setMode({ type: 'server_error_500', message: 'primary deliberate 500' })
    upstreams.secondary.setMode({ type: 'success', delayMs: 20, text: 'secondary recovery' })
    upstreams.tertiary.setMode({ type: 'success', delayMs: 20, text: 'tertiary recovery' })
    const errorBurst = await runBatch(requestKey, {
      name: 'error-burst',
      count: 3,
      concurrency: 1,
      path: REQUEST_PATH,
      stream: false,
      promptPrefix: 'error',
      interRequestDelayMs: 1200,
    })
    createAssertions('error-burst', errorBurst, upstreams, {
      successMin: 2,
      failMax: 1,
      primaryHitsMin: 1,
      secondaryHitsMin: 1,
    })

    upstreams.primary.setMode({ type: 'server_error_500', message: 'primary tertiary failover 500' })
    upstreams.secondary.setMode({ type: 'server_error_500', message: 'secondary tertiary failover 500' })
    upstreams.tertiary.setMode({ type: 'success', delayMs: 20, text: 'tertiary failover' })
    const tertiaryFailover = await runBatch(requestKey, {
      name: 'tertiary-failover',
      count: 6,
      concurrency: 2,
      path: REQUEST_PATH,
      stream: false,
      promptPrefix: 'tertiary',
      interRequestDelayMs: 1200,
    })
    createAssertions('tertiary-failover', tertiaryFailover, upstreams, {
      successMin: 5,
      failMax: 1,
      tertiaryHitsMin: 1,
    })

    await delay(1200)
    upstreams.primary.setMode({ type: 'success', delayMs: 40, text: 'primary recovered' })
    upstreams.secondary.setMode({ type: 'success', delayMs: 20, text: 'secondary recovered' })
    const recovery = await runBatch(requestKey, {
      name: 'recovery',
      count: 10,
      concurrency: 2,
      path: REQUEST_PATH,
      stream: true,
      promptPrefix: 'recovery',
    })
    createAssertions('recovery', recovery, upstreams, {
      successMin: 10,
      failMax: 0,
      primaryHitsMin: 1,
    })

    upstreams.primary.setMode({ type: 'rate_limit_429', message: 'primary deliberate 429' })
    upstreams.secondary.setMode({ type: 'success', delayMs: 20, text: 'secondary recovery' })
    const rateBurst = await runBatch(requestKey, {
      name: 'rate-burst',
      count: 3,
      concurrency: 1,
      path: REQUEST_PATH,
      stream: true,
      promptPrefix: 'rate',
      interRequestDelayMs: 1200,
    })
    createAssertions('rate-burst', rateBurst, upstreams, {
      successMin: 2,
      failMax: 1,
      secondaryHitsMin: 1,
    })

    upstreams.primary.setMode({ type: 'success', delayMs: 40, text: 'primary recovered again' })
    const nonStream = await runBatch(requestKey, {
      name: 'non-stream',
      count: 4,
      concurrency: 1,
      path: REQUEST_PATH,
      stream: false,
      promptPrefix: 'json',
    })
    createAssertions('non-stream', nonStream, upstreams, {
      successMin: 4,
      failMax: 0,
      primaryHitsMin: 1,
    })

    const usageBeforeCc = await getUsageRecords()
    const ccStream = await runBatch(requestKey, {
      name: 'cc-path',
      count: 4,
      concurrency: 2,
      path: CC_REQUEST_PATH,
      stream: true,
      promptPrefix: 'cc-path',
    })
    createAssertions('cc-path', ccStream, upstreams, {
      successMin: 4,
      failMax: 0,
      primaryHitsMin: 1,
    })

    await delay(1000)
    const usageAfterCc = await getUsageRecords()
    const ccRecordCount = Math.max(0, (usageAfterCc.total || 0) - (usageBeforeCc.total || 0))
    const ccRecords = (usageAfterCc.records || []).slice(0, ccRecordCount)
    assert.ok(ccRecordCount >= 4, 'expected new usage records for /cc path')
    assert.ok(
      ccRecords.every((record) => record.routeKind === 'account'),
      'cc path records should be routed through accounts',
    )
    assert.ok(
      ccRecords.some((record) => record.usageProjectionApplied === true),
      'expected at least one cc path record with usage projection applied',
    )
    assert.ok(
      ccRecords.every((record) => [primary.id, secondary.id, tertiary.id].includes(record.accountId ?? 0)),
      'cc path records should reference one of the mock accounts',
    )
    assert.ok(
      ccRecords.every((record) => Array.isArray(record.accountAttempts) && record.accountAttempts.length >= 1),
      'cc path records should preserve account attempts',
    )

    const summary = {
      loadBalancingMode: await getLoadBalancingMode(),
      requestKeyId,
      accounts: {
        primary: primary.id,
        secondary: secondary.id,
        tertiary: tertiary.id,
      },
      upstreamHits: Object.fromEntries(Object.entries(upstreams).map(([key, upstream]) => [key, upstream.state.hits])),
      usageRecords: usageAfterCc.total,
      latestUsage: {
        endpoint: ccRecords[0]?.endpoint,
        routeKind: ccRecords[0]?.routeKind,
        accountId: ccRecords[0]?.accountId,
        usageProjectionApplied: ccRecords[0]?.usageProjectionApplied,
      },
    }
    console.log(JSON.stringify(summary, null, 2))
  } finally {
    upstreams.primary.setMode({ type: 'success', delayMs: 0, text: 'primary ok' })
    upstreams.secondary.setMode({ type: 'success', delayMs: 0, text: 'secondary ok' })
    upstreams.tertiary.setMode({ type: 'success', delayMs: 0, text: 'tertiary ok' })

    for (const account of createdAccounts.reverse()) {
      try {
        await deleteAccount(account.id)
      } catch (error) {
        console.warn(`failed to delete mock account ${account.id}: ${error?.message || error}`)
      }
    }

    for (const account of disabledAccounts.reverse()) {
      try {
        await setAccountEnabled(account.id, true)
      } catch (error) {
        console.warn(`failed to restore account ${account.id}: ${error?.message || error}`)
      }
    }

    try {
      const current = await getRuntimeConfig()
      const restoreConfig = {
        ...current,
        accountRuntime: {
          ...current.accountRuntime,
          ...originalExternalPools,
        },
        externalPools: {
          ...current.externalPools,
          ...originalExternalPools,
        },
      }
      await updateRuntimeConfig(restoreConfig)
    } catch (error) {
      console.warn(`failed to restore runtime config: ${error?.message || error}`)
    }

    try {
      await setLoadBalancingMode(originalLoadBalancingMode || 'balanced')
    } catch (error) {
      console.warn(`failed to restore load balancing mode: ${error?.message || error}`)
    }

    for (const key of createdRequestKeys.reverse()) {
      try {
        await deleteRequestKey(key.id)
      } catch (error) {
        console.warn(`failed to delete request key ${key.id}: ${error?.message || error}`)
      }
    }

    await Promise.all(Object.values(upstreams).map((upstream) => upstream.close()))
  }
}

main().catch((error) => {
  console.error(error)
  process.exit(1)
})
