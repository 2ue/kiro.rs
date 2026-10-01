// E2E 使用的模拟管理 API 数据：只覆盖冒烟流程需要的接口，其余返回空结构
const now = new Date().toISOString()
const cred = (id, email, extra = {}) => ({
  id, email, createdAt: now, updatedAt: now, priority: 0, disabled: false, failureCount: 0, isCurrent: false, expiresAt: null,
  authMethod: 'social', effectiveAuthRegion: 'us-east-1', effectiveApiRegion: 'us-east-1', hasProfileArn: true,
  successCount: 10, lastUsedAt: now, hasProxy: false, effectiveProxySource: 'none', refreshFailureCount: 0, endpoint: 'ide',
  cooledDown: false, cooldownRemainingSecs: 0, cooldowns: [], rateLimited: false, rateLimitRemainingSecs: 0, inFlightRequests: 0,
  oldestInFlightAgeSecs: 0, newestInFlightIdleSecs: 0, maxConcurrentRequests: 4, inFlightLeaseMaxSecs: 0, warmupRemaining: 0,
  estimatedCostUsd: 1.5, originalCostUsd: 2, kiroMeteringUsage: 10, pricedRequests: 10, unpricedRequests: 0, rpm: 0,
  rateLimitAutoDisableEnabled: false, tags: [], recentErrorRate: 0, schedulerScore: 0.1,
  accountInfo: { subscriptionTitle: 'KIRO PRO', currentUsage: 850, usageLimit: 1000, remaining: 150, usagePercentage: 85, creditLimit: 0,
    creditRemaining: 0, creditBase: 0, creditBonus: 0, overageCap: 0, overageRate: 0, currentOverages: 0, nextResetAt: Math.floor(Date.now() / 1000) + 3 * 86400, checkedAt: now },
  ...extra,
})

export function createState() {
  return {
    credentials: [
      cred(1, 'alice@example.com'),
      cred(2, 'bob@example.com', { disabled: true, disabledReason: 'InvalidRefreshToken' }),
      cred(3, 'carol@example.com', { recentErrorRate: 0.8, lastErrorReason: 'ThrottlingException: rate exceeded' }),
    ],
    runtimeConfig: { credentialRpm: 0, streamKeepaliveIntervalSecs: 5, kiroUpstreamRegionRotationEnabled: false, externalPools: { externalPoolsEnabled: false, externalPoolRetryMaxAttempts: 3 } },
    puts: [],
  }
}

export function route(state, method, path, body) {
  const ok = (data) => ({ status: 200, body: data })
  const p = path.replace(/^\/console\/api\/admin|^\/api\/admin/, '')
  if (p === '/system/version') return ok({ version: '0.0.0-e2e' })
  if (p === '/credentials/summary') {
    const c = state.credentials
    return ok({ total: c.length, available: c.filter((x) => !x.disabled).length, disabled: c.filter((x) => x.disabled).length, currentId: 1,
      globalInFlightRequests: 0, queuedRequests: 0, globalMaxConcurrentRequests: 64, maxQueuedRequests: 0, updatedAt: now, runtimeFresh: true,
      schedulable: c.filter((x) => !x.disabled).length, coolingDown: 0, inUse: 0, failing: 1 })
  }
  if (p.startsWith('/credentials-paged')) {
    const u = new URL('http://x' + path)
    let list = state.credentials
    const status = u.searchParams.get('status')
    if (status === 'disabled') list = list.filter((c) => c.disabled)
    if (status === 'enabled') list = list.filter((c) => !c.disabled)
    const id = u.searchParams.get('credentialId')
    if (id) list = list.filter((c) => c.id === Number(id))
    return ok({ total: state.credentials.length, available: 2, currentId: 1, globalInFlightRequests: 0, queuedRequests: 0, globalMaxConcurrentRequests: 64,
      maxQueuedRequests: 0, page: 1, limit: 50, totalPages: 1, filteredTotal: list.length, filteredAvailable: list.length, credentials: list })
  }
  const dis = p.match(/^\/credentials\/(\d+)\/disabled$/)
  if (dis && method === 'POST') {
    const c = state.credentials.find((x) => x.id === Number(dis[1]))
    c.disabled = body.disabled
    c.disabledReason = body.disabled ? 'Manual' : undefined
    return ok({ success: true, message: 'ok' })
  }
  if (p === '/config/runtime' && method === 'GET') return ok(state.runtimeConfig)
  if (p === '/config/runtime' && method === 'PUT') {
    state.puts.push(body)
    state.runtimeConfig = body
    return ok(body)
  }
  if (p === '/config/load-balancing') return ok({ mode: 'priority' })
  if (p === '/usage-writer-stats') return ok({ inMemoryLimit: 1000, inMemoryRecords: 0, redisEnabled: false, redisQueueEnabled: false, redisQueueCapacity: 0, redisQueueAvailable: 0, droppedRedisRecords: 0, postgresEnabled: true, writerQueueEnabled: false, writerQueueCapacity: 0, writerQueueAvailable: 0, droppedPersistRecords: 0 })
  if (p === '/external-pools/status' || p === '/external-pools') return ok({ pools: [] })
  if (p === '/proxy-resources') return ok({ resources: [] })
  if (p.startsWith('/usage-dashboard/windows')) return ok({ generatedAt: now, timezone: 'UTC', windows: [] })
  if (p.startsWith('/usage-dashboard/series')) return ok({ generatedAt: now, timezone: 'UTC', series: { hourly24h: [], daily7d: [] } })
  if (p.startsWith('/usage-dashboard/top'))
    return ok({ generatedAt: now, top: { windowKey: 'today', models: [], credentials: [], endpoints: [], errors: [], modelsTotal: 0, credentialsTotal: 0, endpointsTotal: 0, errorsTotal: 0, modelsTruncated: false, credentialsTruncated: false, endpointsTruncated: false, errorsTruncated: false, orderBy: 'requests', errorsOrderBy: 'requests' } })
  if (p.startsWith('/usage-records-paged')) return ok({ page: 1, limit: 100, hasNext: false, records: [] })
  if (p.startsWith('/credentials/') && p.endsWith('/diagnostics')) return ok({ credentialId: 1, page: 1, limit: 20, hasNext: false, records: [], generatedAt: now })
  if (p === '/model-capabilities') return ok({ available: true, source: 'e2e', modelCount: 0, models: [] })
  return { status: 404, body: { error: { type: 'not_found', message: `e2e mock: ${method} ${p}` } } }
}
