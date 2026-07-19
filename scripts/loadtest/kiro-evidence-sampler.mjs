#!/usr/bin/env node

import net from "node:net";
import tls from "node:tls";
import { execFile } from "node:child_process";
import { mkdir, readdir, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const args = parseArgs(process.argv.slice(2));

if (args.help) {
  printHelp();
  process.exit(0);
}

const config = buildConfig(args);
const activeControllers = new Set();
const activeRedisSockets = new Set();
const pendingDelays = new Map();
let stopRequested = false;
let forceStop = false;
let stopReason = "duration";
let signalCount = 0;

function parseArgs(argv) {
  const parsed = {};
  for (let index = 0; index < argv.length; index += 1) {
    const item = argv[index];
    if (!item.startsWith("--")) continue;
    const key = item
      .slice(2)
      .replace(/-([a-z])/g, (_, character) => character.toUpperCase());
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) {
      parsed[key] = "true";
    } else {
      parsed[key] = value;
      index += 1;
    }
  }
  return parsed;
}

function parseBool(value) {
  return value === true || value === "true" || value === "1" || value === "yes";
}

function parseDuration(value, name) {
  const match = String(value).trim().match(/^(\d+(?:\.\d+)?)(ms|s|m|h)?$/i);
  if (!match) throw new Error(`invalid ${name}: ${value}`);
  const amount = Number.parseFloat(match[1]);
  const unit = (match[2] || "s").toLowerCase();
  if (unit === "ms") return amount;
  if (unit === "s") return amount * 1_000;
  if (unit === "m") return amount * 60_000;
  if (unit === "h") return amount * 3_600_000;
  throw new Error(`invalid ${name}: ${value}`);
}

function parsePositiveInteger(value, name, fallback) {
  const resolved = value ?? fallback;
  const parsed = Number.parseInt(String(resolved), 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

function parseCredentialIds(value) {
  const ids = new Set();
  for (const token of String(value || "1-40").split(",")) {
    const trimmed = token.trim();
    if (!trimmed) continue;
    const range = trimmed.match(/^(\d+)-(\d+)$/);
    if (range) {
      const start = Number.parseInt(range[1], 10);
      const end = Number.parseInt(range[2], 10);
      if (start <= 0 || end < start || end - start > 10_000) {
        throw new Error(`invalid credential id range: ${trimmed}`);
      }
      for (let id = start; id <= end; id += 1) ids.add(id);
      continue;
    }
    const id = Number.parseInt(trimmed, 10);
    if (!Number.isFinite(id) || id <= 0 || String(id) !== trimmed) {
      throw new Error(`invalid credential id: ${trimmed}`);
    }
    ids.add(id);
  }
  return [...ids].sort((left, right) => left - right);
}

function parseOptionalIso(value, name) {
  if (!value) return null;
  const epochMs = Date.parse(value);
  if (!Number.isFinite(epochMs)) throw new Error(`invalid ${name}: ${value}`);
  return new Date(epochMs).toISOString();
}

function buildConfig(parsed) {
  const output = parsed.output || process.env.REPORT;
  if (!output) throw new Error("--output is required");

  const targetPid = parsePositiveInteger(parsed.targetPid, "--target-pid");
  const baseUrl = new URL(
    parsed.baseUrl || process.env.KIRO_BASE_URL || "http://127.0.0.1:19034"
  );
  const adminKey = parsed.adminKey || process.env.KIRO_ADMIN_KEY;
  if (!adminKey) {
    throw new Error("--admin-key or KIRO_ADMIN_KEY is required");
  }

  const durationMs = parseDuration(parsed.duration || "5m", "--duration");
  const intervalMs = parsePositiveInteger(parsed.intervalMs, "--interval-ms", "1000");
  if (intervalMs < 100) throw new Error("--interval-ms must be at least 100");

  const redisUrl = parsed.redisUrl || process.env.KIRO_TEST_REDIS_URL || null;
  if (redisUrl && !["redis:", "rediss:"].includes(new URL(redisUrl).protocol)) {
    throw new Error("--redis-url must use redis:// or rediss://");
  }

  return {
    output,
    targetPid,
    baseUrl,
    adminKey,
    durationMs,
    intervalMs,
    adminTimeoutMs: parsePositiveInteger(
      parsed.adminTimeoutMs,
      "--admin-timeout-ms",
      "5000"
    ),
    credentialIds: parseCredentialIds(
      parsed.credentialIds || process.env.KIRO_CREDENTIAL_IDS || "1-40"
    ),
    redisUrl,
    redisPrefix: String(parsed.redisPrefix || process.env.KIRO_REDIS_PREFIX || "").replace(
      /:+$/,
      ""
    ),
    redisTimeoutMs: parsePositiveInteger(
      parsed.redisTimeoutMs,
      "--redis-timeout-ms",
      "1500"
    ),
    usageSince: parseOptionalIso(parsed.usageSince, "--usage-since"),
    usageUntil: parseOptionalIso(parsed.usageUntil, "--usage-until"),
    usageEndpoint: parsed.usageEndpoint || "/cc/v1/messages",
    usagePageLimit: parsePositiveInteger(
      parsed.usagePageLimit,
      "--usage-page-limit",
      "500"
    ),
    usageMaxPages: parsePositiveInteger(parsed.usageMaxPages, "--usage-max-pages", "1000"),
    usageDrainTimeoutMs: parseDuration(
      parsed.usageDrainTimeout || "60s",
      "--usage-drain-timeout"
    ),
    usageDrainIntervalMs: parsePositiveInteger(
      parsed.usageDrainIntervalMs,
      "--usage-drain-interval-ms",
      "250"
    ),
    skipUsage: parseBool(parsed.skipUsage || "false"),
    quiet: parseBool(parsed.quiet || "false"),
  };
}

function printHelp() {
  console.log(`Usage: node scripts/loadtest/kiro-evidence-sampler.mjs [options]

Required:
  --target-pid PID                kiro-rs process to sample
  --output PATH                   JSON evidence report
  --admin-key KEY                 admin key (prefer KIRO_ADMIN_KEY)

Sampling:
  --base-url URL                  default http://127.0.0.1:19034
  --duration 5m                   use 0 to run until SIGINT
  --interval-ms 1000
  --credential-ids 1-40           comma-separated ids and ranges
  --admin-timeout-ms 5000

Optional Redis queue sampling:
  --redis-url redis://127.0.0.1:36381/0
  --redis-prefix kiro_rs:c300:20260717
  --redis-timeout-ms 1500

Usage evidence:
  --usage-since RFC3339            defaults to sampler start
  --usage-until RFC3339            defaults to sampler end
  --usage-endpoint /cc/v1/messages
  --usage-page-limit 500
  --usage-max-pages 1000
  --usage-drain-timeout 60s
  --usage-drain-interval-ms 250
  --skip-usage true

The sampler never starts, stops, or sends load to the target service.`);
}

function cancelPendingDelays() {
  for (const [timer, resolve] of pendingDelays) {
    clearTimeout(timer);
    resolve();
  }
  pendingDelays.clear();
}

function abortActiveRequests(reason) {
  for (const controller of activeControllers) {
    controller.abort(new Error(reason));
  }
  activeControllers.clear();
  for (const socket of activeRedisSockets) {
    socket.destroy(new Error(reason));
  }
  activeRedisSockets.clear();
}

function requestStop(signal) {
  signalCount += 1;
  if (signalCount === 1) {
    stopRequested = true;
    stopReason = signal.toLowerCase();
    cancelPendingDelays();
    abortActiveRequests(`${signal} requested`);
    return;
  }
  forceStop = true;
  cancelPendingDelays();
  abortActiveRequests(`${signal} forced`);
}

function delay(ms) {
  if (ms <= 0 || forceStop) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      pendingDelays.delete(timer);
      resolve();
    }, ms);
    pendingDelays.set(timer, resolve);
  });
}

async function getJson(pathname, query = null, timeoutMs = config.adminTimeoutMs) {
  const url = new URL(pathname, config.baseUrl);
  if (query) {
    for (const [key, value] of Object.entries(query)) {
      if (value !== null && value !== undefined && value !== "") {
        url.searchParams.set(key, String(value));
      }
    }
  }

  const controller = new AbortController();
  const timeout = setTimeout(
    () => controller.abort(new Error(`admin request timed out after ${timeoutMs}ms`)),
    timeoutMs
  );
  activeControllers.add(controller);
  try {
    const response = await fetch(url, {
      headers: { "x-api-key": config.adminKey },
      signal: controller.signal,
    });
    if (!response.ok) throw new Error(`${url.pathname}: HTTP ${response.status}`);
    return await response.json();
  } finally {
    clearTimeout(timeout);
    activeControllers.delete(controller);
  }
}

async function sampleProcess(pid) {
  const processStats = execFileAsync(
    "ps",
    ["-o", "rss=", "-o", "%cpu=", "-p", String(pid)],
    { maxBuffer: 1024 * 1024 }
  );
  const fdStats = sampleFdCount(pid);
  const [{ stdout }, fdCount] = await Promise.all([processStats, fdStats]);
  const values = stdout.trim().split(/\s+/);
  if (values.length < 2) throw new Error(`process ${pid} is not available`);
  const rssKb = Number.parseInt(values[0], 10);
  const cpuPercent = Number.parseFloat(values[1]);
  if (!Number.isFinite(rssKb) || !Number.isFinite(cpuPercent)) {
    throw new Error(`unable to parse ps output for process ${pid}`);
  }
  return { pid, rssBytes: rssKb * 1024, cpuPercent, fdCount };
}

async function sampleFdCount(pid) {
  if (process.platform === "linux") {
    return (await readdir(`/proc/${pid}/fd`)).length;
  }
  const { stdout } = await execFileAsync("lsof", ["-nP", "-p", String(pid)], {
    maxBuffer: 16 * 1024 * 1024,
  });
  return Math.max(0, stdout.trimEnd().split("\n").length - 1);
}

function aggregateRuntime(runtime) {
  const items = runtime?.items || [];
  const sum = (field) => items.reduce((total, item) => total + (Number(item[field]) || 0), 0);
  const count = (field) => items.filter((item) => Boolean(item[field])).length;
  const max = (field) =>
    items.reduce((value, item) => Math.max(value, Number(item[field]) || 0), 0);
  const rolling60s = items.map(
    (item) => Number(item.recentSchedulerSelectionCount60s) || 0
  );
  return {
    fresh: runtime?.fresh ?? false,
    itemCount: items.length,
    inFlight: sum("inFlightRequests"),
    maxCredentialInFlight: max("inFlightRequests"),
    successCount: sum("successCount"),
    schedulerSelectionCount: sum("schedulerSelectionCount"),
    recentSelections10s: sum("recentSchedulerSelectionCount10s"),
    recentSelections60s: sum("recentSchedulerSelectionCount60s"),
    selection60sMin: rolling60s.length ? Math.min(...rolling60s) : 0,
    selection60sMax: rolling60s.length ? Math.max(...rolling60s) : 0,
    cooledDown: count("cooledDown"),
    rateLimited: count("rateLimited"),
    failureCount: sum("failureCount"),
    refreshFailureCount: sum("refreshFailureCount"),
    transientFailureStreak: sum("transientFailureStreak"),
  };
}

function normalizeExternalPools(response) {
  return (response?.pools || []).map((item) => {
    const pool = item.pool || item;
    return {
      id: pool.id,
      name: pool.name,
      enabled: pool.enabled,
      autoDisabled: pool.autoDisabled,
      maxConcurrentRequests: pool.maxConcurrentRequests,
      inFlightRequests: item.inFlight ?? item.inFlightRequests ?? pool.inFlightRequests ?? null,
      cooldownRemainingSecs:
        item.cooldownRemainingSecs ?? pool.cooldownRemainingSecs ?? null,
      dispatchable: item.dispatchable ?? pool.dispatchable ?? null,
      skippedReason: item.skippedReason ?? pool.skippedReason ?? null,
    };
  });
}

async function sampleAdmin() {
  const runtimeQuery = config.credentialIds.length
    ? { ids: config.credentialIds.join(",") }
    : null;
  const requests = [
    ["summary", getJson("/api/admin/credentials/summary")],
    ["runtime", getJson("/api/admin/credentials/runtime", runtimeQuery)],
    ["externalPools", getJson("/api/admin/external-pools/status")],
  ];
  const settled = await Promise.allSettled(requests.map(([, promise]) => promise));
  const errors = [];
  const values = {};
  settled.forEach((result, index) => {
    const name = requests[index][0];
    if (result.status === "fulfilled") {
      values[name] = result.value;
    } else {
      errors.push({ source: `admin.${name}`, message: errorMessage(result.reason) });
    }
  });
  return {
    summary: values.summary || null,
    runtime: values.runtime ? aggregateRuntime(values.runtime) : null,
    externalPools: values.externalPools ? normalizeExternalPools(values.externalPools) : null,
    errors,
  };
}

function redisKey(suffix) {
  return config.redisPrefix ? `${config.redisPrefix}:${suffix}` : suffix;
}

function encodeRedisCommand(parts) {
  const chunks = [Buffer.from(`*${parts.length}\r\n`)];
  for (const part of parts) {
    const value = Buffer.from(String(part));
    chunks.push(Buffer.from(`$${value.length}\r\n`), value, Buffer.from("\r\n"));
  }
  return Buffer.concat(chunks);
}

function parseRedisResponse(buffer, offset = 0) {
  if (offset >= buffer.length) return null;
  const prefix = String.fromCharCode(buffer[offset]);
  const lineEnd = buffer.indexOf("\r\n", offset + 1);
  if (lineEnd < 0) return null;
  const line = buffer.subarray(offset + 1, lineEnd).toString("utf8");
  if (prefix === "+") return { value: line, next: lineEnd + 2 };
  if (prefix === "-") return { value: new Error(line), next: lineEnd + 2 };
  if (prefix === ":") return { value: Number.parseInt(line, 10), next: lineEnd + 2 };
  if (prefix === "$") {
    const length = Number.parseInt(line, 10);
    if (length === -1) return { value: null, next: lineEnd + 2 };
    const start = lineEnd + 2;
    const end = start + length;
    if (buffer.length < end + 2) return null;
    return { value: buffer.subarray(start, end).toString("utf8"), next: end + 2 };
  }
  if (prefix === "*") {
    const count = Number.parseInt(line, 10);
    if (count === -1) return { value: null, next: lineEnd + 2 };
    const values = [];
    let next = lineEnd + 2;
    for (let index = 0; index < count; index += 1) {
      const parsed = parseRedisResponse(buffer, next);
      if (!parsed) return null;
      values.push(parsed.value);
      next = parsed.next;
    }
    return { value: values, next };
  }
  throw new Error(`unsupported Redis response prefix: ${prefix}`);
}

async function redisCommandBatch(redisUrlValue, commands, timeoutMs) {
  const redisUrl = new URL(redisUrlValue);
  const connectionCommands = [];
  const username = redisUrl.username ? decodeURIComponent(redisUrl.username) : null;
  const password = redisUrl.password ? decodeURIComponent(redisUrl.password) : null;
  if (password) {
    connectionCommands.push(username ? ["AUTH", username, password] : ["AUTH", password]);
  }
  const database = Number.parseInt(redisUrl.pathname.replace(/^\//, "") || "0", 10);
  if (Number.isFinite(database) && database > 0) connectionCommands.push(["SELECT", database]);
  connectionCommands.push(...commands);

  return await new Promise((resolve, reject) => {
    let settled = false;
    let buffer = Buffer.alloc(0);
    let parsedOffset = 0;
    const responses = [];
    const host = redisUrl.hostname;
    const port = Number.parseInt(redisUrl.port || (redisUrl.protocol === "rediss:" ? "6380" : "6379"), 10);

    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      activeRedisSockets.delete(socket);
      if (error) {
        socket.destroy();
        reject(error);
      } else {
        socket.end();
        resolve(value);
      }
    };
    const onConnect = () => {
      socket.write(Buffer.concat(connectionCommands.map(encodeRedisCommand)));
    };
    const socket =
      redisUrl.protocol === "rediss:"
        ? tls.connect(
            {
              host,
              port,
              ...(net.isIP(host) === 0 ? { servername: host } : {}),
            },
            onConnect
          )
        : net.createConnection({ host, port }, onConnect);
    activeRedisSockets.add(socket);
    const timeout = setTimeout(
      () => finish(new Error(`Redis sample timed out after ${timeoutMs}ms`)),
      timeoutMs
    );
    socket.setNoDelay(true);
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (responses.length < connectionCommands.length) {
        const parsed = parseRedisResponse(buffer, parsedOffset);
        if (!parsed) break;
        responses.push(parsed.value);
        parsedOffset = parsed.next;
      }
      if (responses.length === connectionCommands.length) {
        const commandResponses = responses.slice(connectionCommands.length - commands.length);
        const redisError = responses.find((value) => value instanceof Error);
        finish(redisError || null, commandResponses);
      }
    });
    socket.once("error", (error) => finish(error));
    socket.once("close", () => {
      if (!settled) finish(new Error("Redis connection closed before all responses arrived"));
    });
  });
}

async function sampleRedisQueues() {
  if (!config.redisUrl) return null;
  const [local, external] = await redisCommandBatch(
    config.redisUrl,
    [
      ["ZCARD", redisKey("scheduler:global:queue_leases:v1")],
      ["ZCARD", redisKey("external_pool:global:queue_leases:v1")],
    ],
    config.redisTimeoutMs
  );
  return { local: Number(local) || 0, external: Number(external) || 0 };
}

async function sampleOnce(startedAtMs) {
  const sampledAtMs = Date.now();
  const [processResult, adminResult, redisResult] = await Promise.allSettled([
    sampleProcess(config.targetPid),
    sampleAdmin(),
    sampleRedisQueues(),
  ]);
  const errors = [];
  const processSample = unwrapResult(processResult, "process", errors);
  const adminSample = unwrapResult(adminResult, "admin", errors);
  const redisQueues = unwrapResult(redisResult, "redis", errors);
  if (adminSample?.errors?.length) errors.push(...adminSample.errors);
  return {
    at: new Date(sampledAtMs).toISOString(),
    elapsedMs: sampledAtMs - startedAtMs,
    sampleMs: Date.now() - sampledAtMs,
    process: processSample,
    summary: adminSample?.summary || null,
    runtime: adminSample?.runtime || null,
    externalPools: adminSample?.externalPools || null,
    redisQueues,
    errors,
  };
}

function unwrapResult(result, source, errors) {
  if (result.status === "fulfilled") return result.value;
  errors.push({ source, message: errorMessage(result.reason) });
  return null;
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

function writerStatsSnapshot(stats) {
  return {
    accepting: stats.accepting,
    postgresEnabled: stats.postgresEnabled,
    writerQueueEnabled: stats.writerQueueEnabled,
    writerQueueCapacity: stats.writerQueueCapacity,
    writerQueueAvailable: stats.writerQueueAvailable,
    writerAccepted: stats.writerAccepted,
    writerFinished: stats.writerFinished,
    backpressuredPersistRecords: stats.backpressuredPersistRecords,
    droppedPersistRecords: stats.droppedPersistRecords,
    redisEnabled: stats.redisEnabled,
    redisQueueEnabled: stats.redisQueueEnabled,
    redisQueueCapacity: stats.redisQueueCapacity,
    redisQueueAvailable: stats.redisQueueAvailable,
    redisWriterAccepted: stats.redisWriterAccepted,
    redisWriterFinished: stats.redisWriterFinished,
    backpressuredRedisRecords: stats.backpressuredRedisRecords,
    droppedRedisRecords: stats.droppedRedisRecords,
    rejectedAfterShutdown: stats.rejectedAfterShutdown,
  };
}

function usageWriterIsDrained(stats) {
  const postgresProgressAvailable =
    Number.isFinite(Number(stats.writerAccepted)) && Number.isFinite(Number(stats.writerFinished));
  const redisProgressAvailable =
    Number.isFinite(Number(stats.redisWriterAccepted)) &&
    Number.isFinite(Number(stats.redisWriterFinished));
  const postgresDrained = !stats.postgresEnabled
    ? true
    : postgresProgressAvailable
      ? Number(stats.writerAccepted) === Number(stats.writerFinished)
      : Number(stats.writerQueueAvailable) === Number(stats.writerQueueCapacity);
  const redisDrained = !stats.redisEnabled
    ? true
    : redisProgressAvailable
      ? Number(stats.redisWriterAccepted) === Number(stats.redisWriterFinished)
      : Number(stats.redisQueueAvailable) === Number(stats.redisQueueCapacity);
  return {
    drained: postgresDrained && redisDrained,
    postgresDrained,
    redisDrained,
    basis:
      postgresProgressAvailable && redisProgressAvailable ? "accepted_finished" : "queue_available",
  };
}

async function waitForUsageWriters() {
  const startedAtMs = Date.now();
  const deadline = startedAtMs + config.usageDrainTimeoutMs;
  const polls = [];
  const errors = [];
  let state = { drained: false, postgresDrained: false, redisDrained: false, basis: null };
  while (!forceStop) {
    try {
      const raw = await getJson("/api/admin/usage-writer-stats");
      const stats = writerStatsSnapshot(raw);
      state = usageWriterIsDrained(raw);
      polls.push({ at: new Date().toISOString(), stats, ...state });
      if (state.drained) break;
    } catch (error) {
      errors.push({ at: new Date().toISOString(), message: errorMessage(error) });
    }
    if (Date.now() >= deadline) break;
    await delay(Math.min(config.usageDrainIntervalMs, Math.max(0, deadline - Date.now())));
  }
  return {
    startedAt: new Date(startedAtMs).toISOString(),
    completedAt: new Date().toISOString(),
    timeoutMs: config.usageDrainTimeoutMs,
    pollCount: polls.length,
    errorCount: errors.length,
    errors,
    drained: state.drained,
    timedOut: !state.drained && Date.now() >= deadline,
    basis: state.basis,
    first: polls[0] || null,
    last: polls[polls.length - 1] || null,
  };
}

function increment(object, key) {
  object[key] = (object[key] || 0) + 1;
}

function percentile(values, value) {
  if (!values.length) return 0;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil((value / 100) * sorted.length) - 1)
  );
  return sorted[index];
}

function latencySummary(values) {
  return values.length
    ? {
        count: values.length,
        p50: percentile(values, 50),
        p95: percentile(values, 95),
        p99: percentile(values, 99),
        max: Math.max(...values),
      }
    : null;
}

function routeValue(value) {
  return value === null || value === undefined ? "<null>" : String(value);
}

async function aggregateUsageRoutes(since, until) {
  const aggregate = {
    query: { since, until, endpoint: config.usageEndpoint },
    pagesFetched: 0,
    pageLimit: config.usagePageLimit,
    maxPages: config.usageMaxPages,
    truncated: false,
    apiTotal: null,
    records: 0,
    routeKinds: {},
    routeSubtypes: {},
    fallbackReasons: {},
    directPolicyReasons: {},
    externalPoolIds: {},
    localAttempted: {},
    statuses: {},
    combinations: new Map(),
    firstTokenLatencyMs: [],
    durationMs: [],
  };

  for (let page = 1; page <= config.usageMaxPages && !forceStop; page += 1) {
    const response = await getJson("/api/admin/usage-records-paged", {
      page,
      limit: config.usagePageLimit,
      since,
      until,
      endpoint: config.usageEndpoint,
    });
    const records = response.records || [];
    aggregate.pagesFetched += 1;
    aggregate.apiTotal = response.total ?? aggregate.apiTotal;
    for (const record of records) {
      const routeKind = routeValue(record.routeKind);
      const routeSubtype = routeValue(record.routeSubtype);
      const fallbackReason = routeValue(record.fallbackReason);
      const directPolicyReason = routeValue(record.directPolicyReason);
      const externalPoolId = routeValue(record.externalPoolId);
      const localAttempted = routeValue(record.localAttempted);
      const status = routeValue(record.status);
      increment(aggregate.routeKinds, routeKind);
      increment(aggregate.routeSubtypes, routeSubtype);
      increment(aggregate.fallbackReasons, fallbackReason);
      increment(aggregate.directPolicyReasons, directPolicyReason);
      increment(aggregate.externalPoolIds, externalPoolId);
      increment(aggregate.localAttempted, localAttempted);
      increment(aggregate.statuses, status);
      const key = JSON.stringify([
        routeKind,
        routeSubtype,
        fallbackReason,
        directPolicyReason,
        externalPoolId,
        localAttempted,
      ]);
      let group = aggregate.combinations.get(key);
      if (!group) {
        group = {
          routeKind,
          routeSubtype,
          fallbackReason,
          directPolicyReason,
          externalPoolId,
          localAttempted,
          count: 0,
          statuses: {},
          firstTokenLatencyMs: [],
          durationMs: [],
        };
        aggregate.combinations.set(key, group);
      }
      group.count += 1;
      increment(group.statuses, status);
      if (
        record.firstTokenLatencyMs !== null &&
        record.firstTokenLatencyMs !== undefined &&
        Number.isFinite(Number(record.firstTokenLatencyMs))
      ) {
        const latency = Number(record.firstTokenLatencyMs);
        group.firstTokenLatencyMs.push(latency);
        aggregate.firstTokenLatencyMs.push(latency);
      }
      if (Number.isFinite(Number(record.durationMs))) {
        const duration = Number(record.durationMs);
        group.durationMs.push(duration);
        aggregate.durationMs.push(duration);
      }
      aggregate.records += 1;
    }
    if (!response.hasNext || records.length === 0) break;
    if (page === config.usageMaxPages) aggregate.truncated = true;
  }

  const combinations = [...aggregate.combinations.values()]
    .map((group) => ({
      routeKind: group.routeKind,
      routeSubtype: group.routeSubtype,
      fallbackReason: group.fallbackReason,
      directPolicyReason: group.directPolicyReason,
      externalPoolId: group.externalPoolId,
      localAttempted: group.localAttempted,
      count: group.count,
      statuses: group.statuses,
      firstTokenLatencyMs: latencySummary(group.firstTokenLatencyMs),
      durationMs: latencySummary(group.durationMs),
    }))
    .sort((left, right) => right.count - left.count);
  return {
    query: aggregate.query,
    pagesFetched: aggregate.pagesFetched,
    pageLimit: aggregate.pageLimit,
    maxPages: aggregate.maxPages,
    truncated: aggregate.truncated,
    apiTotal: aggregate.apiTotal,
    records: aggregate.records,
    routeKinds: aggregate.routeKinds,
    routeSubtypes: aggregate.routeSubtypes,
    fallbackReasons: aggregate.fallbackReasons,
    directPolicyReasons: aggregate.directPolicyReasons,
    externalPoolIds: aggregate.externalPoolIds,
    localAttempted: aggregate.localAttempted,
    statuses: aggregate.statuses,
    firstTokenLatencyMs: latencySummary(aggregate.firstTokenLatencyMs),
    durationMs: latencySummary(aggregate.durationMs),
    combinations,
  };
}

function metricSummary(samples, selector) {
  const values = samples.map(selector).filter((value) => Number.isFinite(value));
  return values.length
    ? { start: values[0], peak: Math.max(...values), end: values[values.length - 1] }
    : null;
}

function summarizeSamples(samples) {
  return {
    sampleCount: samples.length,
    sampleErrorCount: samples.reduce((total, sample) => total + sample.errors.length, 0),
    rssBytes: metricSummary(samples, (sample) => sample.process?.rssBytes),
    cpuPercent: metricSummary(samples, (sample) => sample.process?.cpuPercent),
    fileDescriptors: metricSummary(samples, (sample) => sample.process?.fdCount),
    localQueuedRequests: metricSummary(samples, (sample) => sample.summary?.queuedRequests),
    localGlobalInFlight: metricSummary(
      samples,
      (sample) => sample.summary?.globalInFlightRequests
    ),
    localRuntimeInFlight: metricSummary(samples, (sample) => sample.runtime?.inFlight),
    localMaxCredentialInFlight: metricSummary(
      samples,
      (sample) => sample.runtime?.maxCredentialInFlight
    ),
    localRecentStarts10s: metricSummary(
      samples,
      (sample) => sample.runtime?.recentSelections10s
    ),
    localRecentStarts60s: metricSummary(
      samples,
      (sample) => sample.runtime?.recentSelections60s
    ),
    externalInFlight: metricSummary(samples, (sample) => {
      if (!sample.externalPools) return undefined;
      return sample.externalPools.reduce(
        (total, pool) => total + (Number(pool.inFlightRequests) || 0),
        0
      );
    }),
    redisLocalQueue: metricSummary(samples, (sample) => sample.redisQueues?.local),
    redisExternalQueue: metricSummary(samples, (sample) => sample.redisQueues?.external),
  };
}

function sanitizedUrl(value) {
  if (!value) return null;
  const url = new URL(value);
  url.username = "";
  url.password = "";
  url.search = "";
  url.hash = "";
  return url.toString();
}

async function runSampler() {
  const signalHandler = (signal) => requestStop(signal);
  const onSigint = () => signalHandler("SIGINT");
  const onSigterm = () => signalHandler("SIGTERM");
  process.on("SIGINT", onSigint);
  process.on("SIGTERM", onSigterm);

  const startedAtMs = Date.now();
  const startedAt = new Date(startedAtMs).toISOString();
  const deadline = config.durationMs > 0 ? startedAtMs + config.durationMs : Number.POSITIVE_INFINITY;
  const samples = [];
  let nextSampleAt = startedAtMs;

  try {
    while (!stopRequested && Date.now() <= deadline) {
      const sample = await sampleOnce(startedAtMs);
      samples.push(sample);
      if (!config.quiet && samples.length % Math.max(1, Math.round(30_000 / config.intervalMs)) === 0) {
        console.error(
          JSON.stringify({
            event: "sample_progress",
            sampleCount: samples.length,
            elapsedMs: Date.now() - startedAtMs,
            sampleErrorCount: samples.reduce((total, item) => total + item.errors.length, 0),
          })
        );
      }
      nextSampleAt += config.intervalMs;
      if (nextSampleAt < Date.now() - config.intervalMs) nextSampleAt = Date.now();
      await delay(Math.max(0, Math.min(nextSampleAt, deadline) - Date.now()));
    }

    if (!stopRequested) stopReason = "duration";
    const endedAtMs = Date.now();
    const endedAt = new Date(endedAtMs).toISOString();
    let usageWriterDrain = null;
    let usageRoutes = null;
    let usageError = null;
    if (!config.skipUsage && !forceStop) {
      try {
        usageWriterDrain = await waitForUsageWriters();
        const since = config.usageSince || startedAt;
        const until = config.usageUntil || endedAt;
        usageRoutes = await aggregateUsageRoutes(since, until);
      } catch (error) {
        usageError = errorMessage(error);
      }
    }

    const report = {
      schemaVersion: 1,
      startedAt,
      endedAt,
      completedAt: new Date().toISOString(),
      stopReason,
      forceStopped: forceStop,
      durationMs: endedAtMs - startedAtMs,
      config: {
        baseUrl: sanitizedUrl(config.baseUrl.toString()),
        targetPid: config.targetPid,
        requestedDurationMs: config.durationMs,
        intervalMs: config.intervalMs,
        adminTimeoutMs: config.adminTimeoutMs,
        credentialIds: config.credentialIds,
        redisUrl: sanitizedUrl(config.redisUrl),
        redisPrefix: config.redisPrefix || null,
        redisTimeoutMs: config.redisTimeoutMs,
        usageSince: config.usageSince,
        usageUntil: config.usageUntil,
        usageEndpoint: config.usageEndpoint,
        usageDrainTimeoutMs: config.usageDrainTimeoutMs,
      },
      summary: summarizeSamples(samples),
      samples,
      usageWriterDrain,
      usageRoutes,
      usageError,
    };

    await mkdir(dirname(config.output), { recursive: true });
    await writeFile(config.output, `${JSON.stringify(report, null, 2)}\n`, {
      encoding: "utf8",
      flag: "wx",
    });
    console.log(
      JSON.stringify({
        output: config.output,
        startedAt,
        endedAt,
        sampleCount: samples.length,
        sampleErrorCount: report.summary.sampleErrorCount,
        usageRecords: usageRoutes?.records ?? null,
        usageDrained: usageWriterDrain?.drained ?? null,
      })
    );
  } finally {
    cancelPendingDelays();
    abortActiveRequests("sampler shutdown");
    process.off("SIGINT", onSigint);
    process.off("SIGTERM", onSigterm);
  }
}

try {
  await runSampler();
} catch (error) {
  cancelPendingDelays();
  abortActiveRequests("fatal sampler error");
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exitCode = 1;
}
