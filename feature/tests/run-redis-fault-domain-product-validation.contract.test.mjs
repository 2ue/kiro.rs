import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const RUNNER = path.join(ROOT, 'feature/tests/run-redis-fault-domain-product-validation.mjs')
const STATIC_BUSINESS_URL = 'redis://127.0.0.1:1/15'
const STATIC_OBSERVABILITY_URL = 'redis://127.0.0.1:2/15'
const LIVE_BUSINESS_URL = String(process.env.KIRO_REDIS_FAULT_DOMAIN_CONTRACT_BUSINESS_URL || '').trim()
const LIVE_OBSERVABILITY_URL = String(
  process.env.KIRO_REDIS_FAULT_DOMAIN_CONTRACT_OBSERVABILITY_URL || '',
).trim()

function runnerEnvironment(overrides = {}) {
  const env = { ...process.env }
  for (const name of [
    'KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL',
    'KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL',
    'KIRO_RS_TEST_REDIS_ISOLATED',
    'KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS',
    'KIRO_REDIS_FAULT_DOMAIN_SCOPE',
    'KIRO_REDIS_FAULT_DOMAIN_TEST_READY_FILE',
  ]) delete env[name]
  Object.assign(env, {
    PATH: process.env.PATH || '/usr/bin:/bin',
    TMPDIR: process.env.TMPDIR || os.tmpdir(),
    KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL: STATIC_BUSINESS_URL,
    KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL: STATIC_OBSERVABILITY_URL,
    KIRO_RS_TEST_REDIS_ISOLATED: '1',
  })
  for (const [key, value] of Object.entries(overrides)) {
    if (value === undefined) delete env[key]
    else env[key] = value
  }
  return env
}

function runRunner(overrides = {}, options = {}) {
  const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-redis-fault-domain-contract-'))
  try {
    const result = spawnSync(process.execPath, [RUNNER], {
      cwd: ROOT,
      env: runnerEnvironment({ TMPDIR: fixtureRoot, ...overrides }),
      encoding: 'utf8',
      timeout: options.timeout ?? 10_000,
      maxBuffer: 2 * 1024 * 1024,
    })
    assert.deepEqual(fs.readdirSync(fixtureRoot), [], 'runner left owned temporary files')
    return result
  } finally {
    fs.rmSync(fixtureRoot, { recursive: true, force: true })
  }
}

function listeningPids(port) {
  const result = spawnSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
  })
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`lsof failed for owned port ${port}: ${result.stderr}`)
  }
  return String(result.stdout || '').split(/\s+/).filter(Boolean).map(Number)
}

function sourceFile(relativePath) {
  return fs.readFileSync(path.join(ROOT, relativePath), 'utf8')
}

function sourceWindow(source, marker, length = 1_200) {
  const start = source.indexOf(marker)
  assert.notEqual(start, -1, `missing source marker: ${marker}`)
  return source.slice(start, start + length)
}

async function waitFor(predicate, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const value = predicate()
    if (value) return value
    await new Promise((resolve) => setTimeout(resolve, 25))
  }
  throw new Error('condition did not become true before timeout')
}

function waitForExit(child, timeoutMs = 15_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`runner ${child.pid} did not exit`)), timeoutMs)
    child.once('error', (error) => {
      clearTimeout(timer)
      reject(error)
    })
    child.once('exit', (code, signal) => {
      clearTimeout(timer)
      resolve({ code, signal })
    })
  })
}

test('product fault-domain runner does not invoke Docker, flush DBs, or probe protected 9022', () => {
  const source = fs.readFileSync(RUNNER, 'utf8')
  assert.doesNotMatch(source, /spawn\([^)]*docker/i)
  assert.doesNotMatch(source, /FLUSHDB|FLUSHALL|--allow-flush/)
  assert.match(source, /port 9022/)
  assert.doesNotMatch(source, /listeningPids\(9022\)/)
  assert.doesNotMatch(source, /-iTCP:9022/)
  assert.match(source, /run-cargo-scoped\.sh/)
})

test('production startup injects only observability Redis into usage and Admin surfaces', () => {
  const source = sourceFile('src/main.rs')
  const usageBlock = sourceWindow(source, 'UsageRecorder::with_postgres_and_observability_redis', 700)
  assert.match(usageBlock, /observability_redis_store\.clone\(\)/)
  assert.doesNotMatch(usageBlock, /\bredis_store\.clone\(\)/)

  const adminBlock = sourceWindow(source, 'admin::AdminService::new(admin::AdminServiceDependencies', 1_200)
  assert.match(adminBlock, /observability_redis_store:\s*observability_redis_store\.clone\(\)/)
  assert.doesNotMatch(adminBlock, /observability_redis_store:\s*redis_store\.clone\(\)/)
})

test('production scheduler, external pool, runtime event, and health paths keep business Redis only', () => {
  const source = sourceFile('src/main.rs')
  const managerBlock = sourceWindow(source, 'MultiTokenManager::new_with_stores_and_runtime_state', 1_200)
  assert.match(managerBlock, /Some\(redis_store\.clone\(\)\)/)
  assert.doesNotMatch(managerBlock, /observability_redis_store/)

  const externalBlock = sourceWindow(source, 'ExternalPoolManager::new', 500)
  assert.match(externalBlock, /postgres_store\.clone\(\),\s*redis_store\.clone\(\)/s)
  assert.doesNotMatch(externalBlock, /observability_redis_store/)

  const eventsBlock = sourceWindow(source, 'spawn_redis_runtime_event_listener(', 900)
  assert.match(eventsBlock, /redis_store\.clone\(\)/)
  assert.doesNotMatch(eventsBlock, /observability_redis_store/)

  const healthBlock = sourceWindow(source, 'let health_state = Arc::new(AppHealthState', 500)
  assert.match(healthBlock, /redis_store:\s*redis_store\.clone\(\)/)
  assert.doesNotMatch(healthBlock, /observability_redis_store/)

  const healthStruct = sourceWindow(source, 'struct AppHealthState', 500)
  assert.match(healthStruct, /redis_store: Arc<RedisStore>/)
  assert.doesNotMatch(healthStruct, /observability/)
})

test('observability Redis is optional and never falls back to business Redis on startup failure', () => {
  const source = sourceFile('src/main.rs')
  const startupBlock = sourceWindow(source, 'let observability_redis_store = if', 3_500)
  assert.match(startupBlock, /RedisStore::connect_observability/)
  assert.match(startupBlock, /observability_redis_enabled = false/)
  assert.match(startupBlock, /不回落到业务 Redis/)
  assert.doesNotMatch(startupBlock, /Some\(redis_store\.clone\(\)\)/)
})

test('UsageRecorder production constructor rejects business Redis and legacy Redis constructor is test-only', () => {
  const source = sourceFile('src/anthropic/usage.rs')
  assert.match(source, /#\[cfg\(test\)\]\s+pub\(crate\)\s+fn with_postgres_and_redis/)
  const constructor = sourceWindow(source, 'pub fn with_postgres_and_observability_redis', 900)
  assert.match(constructor, /is_observability\(\)/)
  assert.match(constructor, /UsageRecorder observability materialization must not use business Redis/)
  assert.match(
    constructor,
    /Self::with_postgres_internal\(limit,\s*postgres_store,\s*observability_redis_store\)/s,
  )
})

test('UsageRecorder request path only enqueues observability Redis writes and drops on pressure', () => {
  const source = sourceFile('src/anthropic/usage.rs')
  const recordBlock = sourceWindow(source, 'pub fn record(&self, record: UsageRecord)', 2_400)
  assert.match(recordBlock, /self\.record_usage_postgres\(record\.clone\(\)\)/)
  assert.match(recordBlock, /self\.record_usage_redis\(record\)/)
  assert.doesNotMatch(recordBlock, /block_on_usage_store|record_usage_summary/)

  const redisEnqueueBlock = sourceWindow(source, 'fn record_usage_redis', 1_400)
  assert.match(redisEnqueueBlock, /writer\.enqueue\(record\)/)
  assert.match(redisEnqueueBlock, /UsageWriterEnqueueError::Full/)
  assert.match(redisEnqueueBlock, /避免阻塞主请求/)
  assert.doesNotMatch(redisEnqueueBlock, /block_on_usage_store|record_usage_summary/)

  const redisWorkerBlock = sourceWindow(source, 'async fn usage_redis_writer_loop', 1_200)
  assert.match(redisWorkerBlock, /persist_usage_redis_batch_with_timeout/)
  const persistBlock = sourceWindow(source, 'async fn persist_usage_redis_batch_with_timeout', 1_200)
  assert.match(persistBlock, /run_bounded_usage_batch/)
  assert.match(persistBlock, /redis\.record_usage_summary\(&record\)/)
})

test('Admin cache and cleanup paths are wired to observability Redis only', () => {
  const source = sourceFile('src/admin/service.rs')
  const constructor = sourceWindow(source, 'pub fn new(dependencies: AdminServiceDependencies)', 1_500)
  assert.match(constructor, /observability_redis_store[\s\S]*is_observability\(\)/)
  assert.match(constructor, /Admin observability caches must not receive the business Redis store/)

  const spawnCleanup = sourceWindow(source, 'fn spawn_usage_cleanup_job', 1_000)
  assert.match(spawnCleanup, /let observability_redis = self\.observability_redis_store\.clone\(\)/)
  assert.doesNotMatch(spawnCleanup, /\bredis_store\b/)

  const cleanupJob = sourceWindow(source, 'async fn run_usage_cleanup_job', 5_000)
  assert.match(cleanupJob, /observability_redis:\s*Option<Arc<RedisStore>>/)
  assert.match(source, /never fall back to the business scheduler Redis/)
  assert.match(source, /if let Some\(redis\) = observability_redis\.as_deref\(\)/)
})

test('RedisStore roles keep business and observability connection paths explicit', () => {
  const source = sourceFile('src/storage/redis_cache.rs')
  const businessConnect = sourceWindow(source, 'pub async fn connect(config: &Config)', 800)
  assert.match(businessConnect, /Self::connect_config\(&config\.redis\)\.await/)
  const explicitBusiness = sourceWindow(source, 'pub async fn connect_config(config: &RedisConfig)', 600)
  assert.match(explicitBusiness, /RedisStoreRole::Business/)
  const observabilityConnect = sourceWindow(source, 'pub async fn connect_observability', 600)
  assert.match(observabilityConnect, /RedisStoreRole::Observability/)
})

test('RedisStore usage materialization entrypoints fail closed away from business Redis in production', () => {
  const source = sourceFile('src/storage/redis_cache.rs')
  const guard = sourceWindow(source, 'fn ensure_observability_usage_store', 500)
  assert.match(guard, /cfg!\(not\(test\)\)/)
  assert.match(guard, /!self\.is_observability\(\)/)
  assert.match(guard, /business scheduler Redis cannot be used for usage materialization/)

  for (const marker of [
    'pub async fn advance_usage_cleanup_watermark',
    'pub async fn invalidate_usage_derived_cache',
    'pub async fn record_usage_summary',
    'pub async fn usage_records_page',
    'pub async fn usage_summary',
    'pub async fn usage_dashboard_series_only',
    'pub async fn usage_dashboard_top_only',
    'pub async fn clear_usage_summary_aggregates_bounded',
    'pub async fn clear_usage_record_snapshots_bounded',
  ]) {
    assert.match(
      sourceWindow(source, marker, 500),
      /ensure_observability_usage_store\(/,
      `${marker} must guard its Redis role`,
    )
  }

  const dashboard = sourceWindow(source, 'pub async fn usage_dashboard', 500)
  assert.match(dashboard, /ensure_observability_usage_store\("read usage dashboard"\)/)
})

test('configuration rejects DB or prefix only Redis separation and supports observability env authority', () => {
  const source = sourceFile('src/model/config.rs')
  const validator = sourceWindow(source, 'pub fn validate_redis_fault_domains', 1_500)
  assert.match(validator, /changing DB or keyPrefix is not sufficient/)
  assert.match(validator, /business == observability/)
  const envOverrides = sourceWindow(source, 'fn apply_env_overrides', 1_500)
  assert.match(envOverrides, /KIRO_RS_OBSERVABILITY_REDIS_URL/)
  assert.match(envOverrides, /KIRO_RS_OBSERVABILITY_REDIS_KEY_PREFIX/)
})

const earlyCases = [
  {
    name: 'missing business Redis URL',
    env: { KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL: undefined },
    error: /KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL is required/,
  },
  {
    name: 'missing observability Redis URL',
    env: { KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL: undefined },
    error: /KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL is required/,
  },
  {
    name: 'isolation marker is absent',
    env: { KIRO_RS_TEST_REDIS_ISOLATED: undefined },
    error: /KIRO_RS_TEST_REDIS_ISOLATED=1 is required/,
  },
  {
    name: 'database zero',
    env: { KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL: 'redis://127.0.0.1:1/0' },
    error: /isolated nonzero Redis database in 1\.\.15/,
  },
  {
    name: 'protected port 9022',
    env: { KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL: 'redis://127.0.0.1:9022/15' },
    error: /protected port 9022/,
  },
  {
    name: 'same authority with different DB',
    env: {
      KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL: 'redis://127.0.0.1:1/14',
      KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL: 'redis://localhost:1/15',
    },
    error: /distinct network authorities/,
  },
  {
    name: 'non-loopback Redis',
    env: { KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL: 'redis://redis.internal:6379/15' },
    error: /must target loopback Redis/,
  },
  {
    name: 'invalid outer rounds',
    env: { KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS: '0' },
    error: /KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS must be an integer in 1\.\.5/,
  },
  {
    name: 'invalid scope',
    env: { KIRO_REDIS_FAULT_DOMAIN_SCOPE: '../bad' },
    error: /KIRO_REDIS_FAULT_DOMAIN_SCOPE has an invalid format/,
  },
]

for (const fixture of earlyCases) {
  for (let round = 1; round <= 3; round += 1) {
    test(`rejects ${fixture.name} before proxy or Cargo, round ${round}`, () => {
      const result = runRunner(fixture.env, { timeout: 5_000 })
      assert.notEqual(result.status, 0, result.stdout)
      const output = `${result.stdout}\n${result.stderr}`
      assert.match(output, fixture.error)
      assert.doesNotMatch(output, /validation-build-admission|cargo test|Redis chaos proxy readiness/)
    })
  }
}

for (const signalCase of [
  { signal: 'SIGHUP', code: 129 },
  { signal: 'SIGINT', code: 130 },
  { signal: 'SIGTERM', code: 143 },
]) {
  for (let round = 1; round <= 3; round += 1) {
    test(`signal cleanup stops owned proxies for ${signalCase.signal}, round ${round}`, {
      skip: !LIVE_BUSINESS_URL || !LIVE_OBSERVABILITY_URL,
    }, async () => {
      const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-redis-fault-domain-signal-'))
      const readyFile = path.join(fixtureRoot, 'ready.json')
      const child = spawn(process.execPath, [RUNNER], {
        cwd: ROOT,
        env: runnerEnvironment({
          TMPDIR: fixtureRoot,
          KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL: LIVE_BUSINESS_URL,
          KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL: LIVE_OBSERVABILITY_URL,
          KIRO_REDIS_FAULT_DOMAIN_TEST_READY_FILE: readyFile,
        }),
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      let stderr = ''
      child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8') })
      const ready = await waitFor(() => {
        if (!fs.existsSync(readyFile)) return null
        return JSON.parse(fs.readFileSync(readyFile, 'utf8'))
      })
      assert.equal(ready.ready, true)
      assert.deepEqual(listeningPids(ready.businessProxyPort), [ready.businessProxyPid])
      assert.deepEqual(listeningPids(ready.observabilityProxyPort), [ready.observabilityProxyPid])
      child.kill(signalCase.signal)
      const exit = await waitForExit(child)
      assert.equal(exit.code, signalCase.code, stderr)
      assert.deepEqual(listeningPids(ready.businessProxyPort), [])
      assert.deepEqual(listeningPids(ready.observabilityProxyPort), [])
      assert.equal(fs.existsSync(ready.tempRoot), false)
      assert.equal(fs.existsSync(readyFile), false)
      fs.rmSync(fixtureRoot, { recursive: true, force: true })
    })
  }
}
