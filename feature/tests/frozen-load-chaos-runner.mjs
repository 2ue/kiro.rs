#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawn } from "node:child_process";

import { resolveRuntimeValidationPaths } from "./runtime-validation-paths.mjs";

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, "../.."));
const { binary: DEFAULT_PRODUCT_BINARY, artifactRoot: ARTIFACT_ROOT } =
  resolveRuntimeValidationPaths(ROOT);
const PROTECTED_PORT = 9022;
const SAFE_ENV_NAMES = [
  "PATH",
  "TMPDIR",
  "TMP",
  "TEMP",
  "LANG",
  "LC_ALL",
  "LC_CTYPE",
  "TZ",
  "USER",
  "LOGNAME",
];

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith("--")) {
      throw new Error(`unexpected positional argument: ${arg}`);
    }
    const key = arg.slice(2);
    const next = argv[i + 1];
    if (next === undefined || next.startsWith("--")) {
      out[key] = true;
    } else {
      out[key] = next;
      i += 1;
    }
  }
  return out;
}

function requirePath(name, value) {
  if (!value || typeof value !== "string") {
    throw new Error(`missing --${name}`);
  }
  const resolved = path.resolve(value);
  if (!fs.existsSync(resolved) || !fs.statSync(resolved).isFile()) {
    throw new Error(`--${name} is not an existing file: ${resolved}`);
  }
  const realPath = fs.realpathSync(resolved);
  const relative = path.relative(ROOT, realPath);
  if (relative && !relative.startsWith("..") && !path.isAbsolute(relative)) {
    throw new Error(`--${name} must be repository-external: ${realPath}`);
  }
  if (isDirectCargoOutputPath(resolved) || isDirectCargoOutputPath(realPath)) {
    throw new Error(`--${name} must be a copied frozen binary, not target/debug or target/release output`);
  }
  return realPath;
}

function isDirectCargoOutputPath(candidate) {
  const segments = path.resolve(candidate).split(path.sep).filter(Boolean).map((value) => (
    value.toLowerCase()
  ));
  return segments.some((segment, index) => (
    segment === "target"
    && (segments[index + 1] === "debug" || segments[index + 1] === "release")
  ));
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || "").trim();
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function minimalEnvironment(extra = {}) {
  const environment = {};
  for (const name of SAFE_ENV_NAMES) {
    if (typeof process.env[name] === "string" && process.env[name] !== "") {
      environment[name] = process.env[name];
    }
  }
  return { ...environment, ...extra };
}

function sha256File(file) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(file));
  return hash.digest("hex");
}

function sha256Text(text) {
  return crypto.createHash("sha256").update(text).digest("hex");
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function freePort() {
  for (;;) {
    const port = await new Promise((resolve, reject) => {
      const server = net.createServer();
      server.on("error", reject);
      server.listen(0, "127.0.0.1", () => {
        const address = server.address();
        server.close(() => resolve(address.port));
      });
    });
    if (port !== PROTECTED_PORT) {
      return port;
    }
  }
}

async function waitTcp(port, label, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      await new Promise((resolve, reject) => {
        const socket = net.createConnection({ host: "127.0.0.1", port }, () => {
          socket.end();
          resolve();
        });
        socket.setTimeout(500, () => {
          socket.destroy(new Error("tcp timeout"));
        });
        socket.on("error", reject);
      });
      return;
    } catch (error) {
      lastError = error;
      await sleep(200);
    }
  }
  throw new Error(`timed out waiting for ${label} on ${port}: ${lastError}`);
}

async function waitReady(port, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let last = "";
  while (Date.now() < deadline) {
    try {
      const { status, body } = await httpGet(`http://127.0.0.1:${port}/readyz`);
      last = `${status} ${body}`;
      if (status === 200 && body.includes('"ready"')) {
        return;
      }
    } catch (error) {
      last = String(error.message || error);
    }
    await sleep(300);
  }
  throw new Error(`proxy did not become ready on ${port}; last=${last}`);
}

function httpGet(url) {
  return new Promise((resolve, reject) => {
    const request = http.get(url, (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => {
        resolve({
          status: response.statusCode,
          body: Buffer.concat(chunks).toString("utf8"),
        });
      });
    });
    request.setTimeout(2_000, () => {
      request.destroy(new Error("http timeout"));
    });
    request.on("error", reject);
  });
}

function spawnLogged(command, args, logPath, options = {}) {
  const out = fs.openSync(logPath, "a");
  const child = spawn(command, args, {
    ...options,
    stdio: ["ignore", out, out],
    detached: false,
  });
  child.once("exit", () => {
    fs.closeSync(out);
  });
  return child;
}

async function terminate(child, label, graceMs = 5_000) {
  if (!child || child.exitCode !== null || child.signalCode !== null) {
    return;
  }
  child.kill("SIGTERM");
  const exited = await waitForExit(child, graceMs).catch(() => false);
  if (!exited && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await waitForExit(child, 5_000).catch(() => false);
  }
  if (child.exitCode === null && child.signalCode === null) {
    throw new Error(`failed to terminate ${label} pid=${child.pid}`);
  }
}

function waitForExit(child, timeoutMs) {
  return new Promise((resolve, reject) => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolve(true);
      return;
    }
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      reject(new Error("timeout"));
    }, timeoutMs);
    function onExit() {
      clearTimeout(timer);
      resolve(true);
    }
    child.once("exit", onExit);
  });
}

function isLoopback(hostname) {
  return hostname === "127.0.0.1" || hostname === "localhost" || hostname === "::1";
}

function requiredServiceCount(tier) {
  if (tier === "l3") return 3;
  if (tier === "l4") return 6;
  if (tier === "l5") return 1;
  throw new Error(`unsupported --tier ${tier}`);
}

function parseDatabaseList(value, expectedCount) {
  const databases = String(value || "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
  if (databases.length !== expectedCount) {
    throw new Error(`KIRO_LOAD_CHAOS_POSTGRES_DATABASES must contain exactly ${expectedCount} pre-created database names`);
  }
  for (const database of databases) {
    if (!/^kiro_load_chaos_[a-z0-9_]{3,80}$/.test(database)) {
      throw new Error("KIRO_LOAD_CHAOS_POSTGRES_DATABASES must contain caller-owned kiro_load_chaos_* names");
    }
  }
  return databases;
}

function validatePostgresTemplate(template, databases) {
  const placeholderCount = (template.match(/\{database\}/g) || []).length;
  if (placeholderCount !== 1) {
    throw new Error("KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE must contain exactly one literal {database} placeholder");
  }
  const parsed = new URL(template.replace("{database}", databases[0] || "kiro_load_chaos_contract_sample"));
  if (!["postgres:", "postgresql:"].includes(parsed.protocol)) {
    throw new Error("KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE must use PostgreSQL");
  }
  if (!isLoopback(parsed.hostname)) {
    throw new Error("KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE must target loopback");
  }
  if (Number(parsed.port || 5432) === PROTECTED_PORT) throw new Error("port 9022 is protected");
  if (parsed.hash) {
    throw new Error("KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE must not contain a fragment");
  }
  return {
    host: parsed.hostname,
    port: Number(parsed.port || 5432),
    template,
  };
}

function dbUrlFromTemplate(template, database) {
  if (!/^kiro_load_chaos_[a-z0-9_]{3,80}$/.test(database)) {
    throw new Error(`unsafe load-chaos database name: ${database}`);
  }
  return template.replace("{database}", database);
}

function validateRedisInput(redisUrl, redisPrefix) {
  const redis = new URL(redisUrl);
  if (redis.protocol !== "redis:") throw new Error("KIRO_LOAD_CHAOS_REDIS_URL must use redis://");
  if (redis.username || redis.password) {
    throw new Error("KIRO_LOAD_CHAOS_REDIS_URL must not contain Redis auth material");
  }
  if (!isLoopback(redis.hostname)) {
    throw new Error("KIRO_LOAD_CHAOS_REDIS_URL must target loopback");
  }
  if (Number(redis.port || 6379) === PROTECTED_PORT) throw new Error("port 9022 is protected");
  if (redis.search || redis.hash) {
    throw new Error("KIRO_LOAD_CHAOS_REDIS_URL must not contain query or fragment data");
  }
  const databaseText = redis.pathname.replace(/^\//, "");
  if (!/^\d+$/.test(databaseText)) {
    throw new Error("KIRO_LOAD_CHAOS_REDIS_URL must name a Redis database");
  }
  const database = Number(databaseText);
  if (!Number.isSafeInteger(database) || database < 1 || database > 15) {
    throw new Error("KIRO_LOAD_CHAOS_REDIS_URL must use an isolated nonzero database in 1..15");
  }
  if (redisPrefix.includes("kiro_rs:local")) {
    throw new Error("KIRO_LOAD_CHAOS_REDIS_PREFIX must be a caller-owned temporary prefix");
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(redisPrefix)) {
    throw new Error("KIRO_LOAD_CHAOS_REDIS_PREFIX has an invalid format");
  }
  return {
    redis,
    port: Number(redis.port || 6379),
    database,
    prefix: redisPrefix,
  };
}

function encodeRedisCommands(commands) {
  return Buffer.concat(commands.map((parts) => {
    const encoded = [Buffer.from(`*${parts.length}\r\n`)];
    for (const part of parts) {
      const bytes = Buffer.from(String(part));
      encoded.push(Buffer.from(`$${bytes.length}\r\n`), bytes, Buffer.from("\r\n"));
    }
    return Buffer.concat(encoded);
  }));
}

function parseRedisReply(buffer, offset = 0) {
  if (offset >= buffer.length) return null;
  const type = String.fromCharCode(buffer[offset]);
  const lineEnd = buffer.indexOf("\r\n", offset + 1);
  if (lineEnd < 0) return null;
  const line = buffer.subarray(offset + 1, lineEnd).toString("utf8");
  const next = lineEnd + 2;
  if (type === "+" || type === "-" || type === ":") {
    return { type, value: type === ":" ? Number(line) : line, next };
  }
  if (type === "$") {
    const length = Number(line);
    if (length === -1) return { type, value: null, next };
    const end = next + length;
    if (end + 2 > buffer.length) return null;
    return { type, value: buffer.subarray(next, end).toString("utf8"), next: end + 2 };
  }
  if (type === "*") {
    const count = Number(line);
    const values = [];
    let cursor = next;
    for (let index = 0; index < count; index += 1) {
      const item = parseRedisReply(buffer, cursor);
      if (!item) return null;
      if (item.type === "-") throw new Error(`Redis command failed: ${item.value}`);
      values.push(item.value);
      cursor = item.next;
    }
    return { type, value: values, next: cursor };
  }
  throw new Error(`unsupported Redis response type ${type}`);
}

function redisCommand(target, command) {
  const commands = [["SELECT", String(target.database)], command];
  const payload = encodeRedisCommands(commands);
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host: target.redis.hostname, port: target.port });
    let received = Buffer.alloc(0);
    let cursor = 0;
    const replies = [];
    let settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      if (error) reject(error);
      else resolve(value);
    };
    socket.setTimeout(5_000);
    socket.once("connect", () => socket.write(payload));
    socket.on("data", (chunk) => {
      received = Buffer.concat([received.subarray(cursor), chunk]);
      cursor = 0;
      try {
        for (;;) {
          const reply = parseRedisReply(received, cursor);
          if (!reply) return;
          if (reply.type === "-") return finish(new Error(`Redis command failed: ${reply.value}`));
          replies.push(reply.value);
          cursor = reply.next;
          if (replies.length === commands.length) return finish(null, replies.at(-1));
        }
      } catch (error) {
        finish(error);
      }
    });
    socket.once("timeout", () => finish(new Error("Redis control command timed out")));
    socket.once("error", (error) => finish(error));
  });
}

async function cleanupRedis(redisTarget, prefix) {
  let cursor = "0";
  let deleted = 0;
  let scanned = 0;
  do {
    const reply = await redisCommand(redisTarget, [
      "SCAN",
      cursor,
      "MATCH",
      `${prefix}*`,
      "COUNT",
      "500",
    ]);
    if (!Array.isArray(reply) || reply.length !== 2 || !Array.isArray(reply[1])) {
      throw new Error("unexpected Redis SCAN reply while cleaning load-chaos prefix");
    }
    cursor = String(reply[0]);
    const keys = reply[1].map((key) => String(key)).filter(Boolean);
    scanned += keys.length;
    for (let index = 0; index < keys.length; index += 250) {
      const chunk = keys.slice(index, index + 250);
      if (chunk.length > 0) {
        const removed = await redisCommand(redisTarget, ["DEL", ...chunk]);
        deleted += Number(removed || 0);
      }
    }
  } while (cursor !== "0");
  return { scanned, deleted };
}

function writeJson(file, value) {
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function serviceConfig({ pgUrl, redisUrl, redisPrefix, proxyPort, fakePort, apiKey }) {
  return {
    postgres: { url: pgUrl },
    redis: { url: redisUrl, keyPrefix: redisPrefix },
    host: "127.0.0.1",
    port: proxyPort,
    apiKey,
    apiKeys: [],
    adminApiKey: `${apiKey}-admin`,
    requestAdmission: {
      rpm: 0,
      maxConcurrentRequests: 500,
      maxQueuedRequests: 1_000,
      queueTimeoutMs: 5_000,
    },
    kiroUpstreamBaseUrl: `http://127.0.0.1:${fakePort}`,
    defaultEndpoint: "cli",
    credentialRpm: 0,
    credentialMaxConcurrentRequests: 500,
    credentialTransientCooldownSecs: 1,
    credentialRateLimitCooldownSecs: 1,
    credentialServerErrorCooldownSecs: 1,
    credentialNetworkErrorCooldownSecs: 1,
    credentialStreamErrorCooldownSecs: 1,
    credentialProtocolErrorCooldownSecs: 1,
    credentialAuthErrorCooldownSecs: 1,
    credentialCooldownBackoffMultiplier: 1.0,
    credentialCooldownJitterPercent: 0,
    credentialProbationSecs: 1,
    credentialMaxCooldownSecs: 3,
    credentialDispatchMaxWaitSecs: 5,
    kiroUpstreamResponseTimeoutSecs: 30,
    kiroUpstreamStreamIdleTimeoutSecs: 8,
    credentialRetryMaxAttempts: 0,
    inferenceUpstreamMaxAttempts: 4,
    auxiliaryUpstreamMaxAttempts: 2,
    auxiliaryUpstreamMaxConcurrentRequests: 16,
    tokenRefreshMaxRpm: 60,
    tokenRefreshBurst: 8,
    dispatchGlobalMaxConcurrentRequests: 500,
    dispatchMaxQueuedRequests: 1_000,
    loadBalancingMode: "balanced",
    bodyConversion: {
      toolSchemaNormalization: true,
      toolNameMapping: true,
      toolSchemaKeyMapping: "sanitize",
      toolSchemaKeyValidationRegex: "^[a-zA-Z0-9_.-]{1,64}$",
      toolChoiceSteering: true,
      chunkedToolPolicy: true,
      thinkingPromptControls: true,
      nativeReasoningFields: true,
      toolPairingRepair: true,
      historyPlaceholderTools: true,
    },
    promptSteering: {
      enabled: true,
      languageEnabled: true,
      taskQualityEnabled: true,
      customEnabled: false,
      toolChoiceEnabled: true,
      thinkingEnabled: true,
      chunkedToolPolicyEnabled: true,
    },
    payloadGuardEnabled: true,
    payloadGuardMode: "on_too_long",
    payloadGuardMaxBytes: 460_800,
    payloadGuardTrimHistory: true,
    payloadGuardExternalEnabled: true,
    externalPools: {
      externalPoolsEnabled: false,
      fallbackOnSchedulerRedisDegraded: true,
      fallbackOnNoAvailableCredentials: true,
      fallbackOnLocalCapacityExhausted: true,
      fallbackOnLocalTransientExhausted: true,
    },
    kiroAgentModeStrategy: "vibe",
  };
}

function fakeCredentials(count = 4) {
  return Array.from({ length: count }, (_, index) => ({
    kiroApiKey: `ksk_loadtest_fake_${index + 1}_${crypto.randomBytes(6).toString("hex")}`,
    authMethod: "api_key",
    endpoint: "cli",
    priority: index,
    maxConcurrentRequests: 500,
    rpm: 0,
    rateLimitAutoDisableEnabled: false,
    supportedModels: ["claude-sonnet-4", "claude-sonnet-4-20250514", "claude-sonnet-4.6"],
  }));
}

async function startFake(ctx, scenario, options = {}) {
  const port = options.port ?? (await freePort());
  const log = path.join(ctx.logsDir, `fake-${ctx.sequence++}-${scenario}.log`);
  const args = [
    "--fake-only",
    "true",
    "--fake-listen",
    `127.0.0.1:${port}`,
    "--scenario",
    scenario,
    "--fake-kiro-eventstream",
    "true",
    "--fake-delay-ms",
    String(options.delayMs ?? 500),
    "--fake-recover-after",
    String(options.recoverAfter ?? 10),
    "--fake-stream-chunks",
    String(options.streamChunks ?? 16),
    "--fake-stream-chunk-delay-ms",
    String(options.streamChunkDelayMs ?? 100),
  ];
  if (options.fakeToolInputChars) {
    args.push("--fake-tool-input-chars", String(options.fakeToolInputChars));
  }
  const child = spawnLogged(ctx.loadtestBinary, args, log, {
    env: minimalEnvironment({ RUST_LOG: "warn" }),
  });
  await waitTcp(port, `fake ${scenario}`);
  ctx.children.push({ child, label: `fake ${scenario}`, log });
  return { port, child, log, scenario };
}

async function startProxy(ctx, fakePort, options = {}) {
  const proxyPort = options.port ?? (await freePort());
  const database = ctx.postgresDatabases[ctx.proxyCounter++];
  if (!database) {
    throw new Error("not enough caller-owned PostgreSQL databases for load-chaos tier");
  }
  ctx.usedDatabases.push(database);
  const redisPrefix = `${ctx.redisPrefix}:db:${database}:`;
  ctx.redisPrefixes.push(redisPrefix);
  const configPath = path.join(ctx.root, `${database}.config.json`);
  const credentialsPath = path.join(ctx.root, `${database}.credentials.json`);
  const apiKey = `sk-load-${database}`;
  writeJson(
    configPath,
    serviceConfig({
      pgUrl: dbUrlFromTemplate(ctx.pgUrlTemplate, database),
      redisUrl: ctx.redisUrl,
      redisPrefix,
      proxyPort,
      fakePort,
      apiKey,
    }),
  );
  writeJson(credentialsPath, fakeCredentials(options.credentials ?? 4));
  const log = path.join(ctx.logsDir, `proxy-${database}.log`);
  const child = spawnLogged(
    ctx.productBinary,
    ["-c", configPath, "--credentials", credentialsPath],
    log,
    {
      env: minimalEnvironment({
        RUST_LOG: "info",
        KIRO_RS_HOST: "127.0.0.1",
        KIRO_RS_PORT: String(proxyPort),
      }),
    },
  );
  await waitTcp(proxyPort, `proxy ${database}`);
  await waitReady(proxyPort);
  ctx.children.push({ child, label: `proxy ${database}`, log });
  return { port: proxyPort, child, log, database, apiKey, redisPrefix };
}

async function restartProxy(ctx, proxy, fakePort) {
  await terminate(proxy.child, `proxy ${proxy.database}`);
  ctx.children = ctx.children.filter((entry) => entry.child !== proxy.child);
  const configPath = path.join(ctx.root, `${proxy.database}.config.json`);
  const credentialsPath = path.join(ctx.root, `${proxy.database}.credentials.json`);
  const existingConfig = JSON.parse(fs.readFileSync(configPath, "utf8"));
  existingConfig.kiroUpstreamBaseUrl = `http://127.0.0.1:${fakePort}`;
  writeJson(configPath, existingConfig);
  const log = path.join(ctx.logsDir, `proxy-${proxy.database}-restart-${ctx.sequence++}.log`);
  const child = spawnLogged(
    ctx.productBinary,
    ["-c", configPath, "--credentials", credentialsPath],
    log,
    {
      env: minimalEnvironment({
        RUST_LOG: "info",
        KIRO_RS_HOST: "127.0.0.1",
        KIRO_RS_PORT: String(proxy.port),
      }),
    },
  );
  await waitTcp(proxy.port, `proxy restart ${proxy.database}`);
  await waitReady(proxy.port);
  ctx.children.push({ child, label: `proxy ${proxy.database} restarted`, log });
  return { ...proxy, child, log };
}

async function restartFake(ctx, fake, scenario, options = {}) {
  await terminate(fake.child, `fake ${fake.scenario}`);
  ctx.children = ctx.children.filter((entry) => entry.child !== fake.child);
  return startFake(ctx, scenario, { ...options, port: fake.port });
}

async function runLoad(ctx, proxy, caseSpec) {
  const reportPath = path.join(ctx.reportsDir, `${caseSpec.id}.json`);
  const logPath = path.join(ctx.logsDir, `load-${caseSpec.id}.log`);
  const args = [
    "--base-url",
    `http://127.0.0.1:${proxy.port}`,
    "--route",
    "/cc/v1/messages",
    "--model",
    "claude-sonnet-4-20250514",
    "--requests",
    String(caseSpec.requests),
    "--concurrency",
    String(caseSpec.concurrency),
    "--scenario",
    caseSpec.clientScenario ?? "normal-stream",
    "--auth-key",
    proxy.apiKey,
    "--target-pid",
    String(proxy.child.pid),
    "--timeout-secs",
    String(caseSpec.timeoutSecs ?? 60),
    "--report",
    reportPath,
  ];
  if (caseSpec.durationSecs) {
    args.push("--duration-secs", String(caseSpec.durationSecs));
  }
  if (caseSpec.stream === false) {
    args.push("--stream", "false");
  }
  if (caseSpec.thinking) {
    args.push("--thinking", "true");
  }
  if (caseSpec.toolUse) {
    args.push("--tool-use", "true");
  }
  if (caseSpec.payloadCase) {
    args.push("--payload-case", caseSpec.payloadCase);
  }
  for (const [flag, value] of Object.entries(caseSpec.extra ?? {})) {
    args.push(`--${flag}`, String(value));
  }
  const child = spawnLogged(ctx.loadtestBinary, args, logPath, {
    env: minimalEnvironment({ RUST_LOG: "warn" }),
  });
  const timeoutMs = (caseSpec.processTimeoutSecs ?? caseSpec.timeoutSecs ?? 60) * 1000 + 10_000;
  const exited = await waitForExit(child, timeoutMs).catch(() => false);
  if (!exited) {
    await terminate(child, `load ${caseSpec.id}`, 1_000);
    throw new Error(`load case timed out: ${caseSpec.id}`);
  }
  if (!fs.existsSync(reportPath)) {
    const logTail = safeTail(logPath);
    throw new Error(`load case did not write report: ${caseSpec.id}\n${logTail}`);
  }
  const reportText = fs.readFileSync(reportPath, "utf8");
  const report = JSON.parse(reportText);
  const reportHash = sha256Text(reportText);
  const logHash = fs.existsSync(logPath) ? sha256File(logPath) : null;
  const result = {
    id: caseSpec.id,
    expect: caseSpec.expect,
    scenario: report.scenario,
    requests: report.requests,
    success: report.success,
    errors: report.errors,
    statusCounts: report.statusCounts,
    ttfbMs: report.ttfbMs,
    firstThinkingMs: report.firstThinkingMs,
    firstTextMs: report.firstTextMs,
    totalLatencyMs: report.totalLatencyMs,
    memory: report.memory,
    fileDescriptors: report.fileDescriptors,
    cpuPercent: report.cpuPercent,
    requestIdSamples: report.requestIds?.slice(0, 5) ?? [],
    errorIdSamples: report.errorIds?.slice(0, 5) ?? [],
    reportSha256: reportHash,
    logSha256: logHash,
    pass: evaluate(caseSpec.expect, report),
  };
  if (!result.pass) {
    result.logTail = safeTail(logPath);
  }
  ctx.results.push(result);
  return result;
}

function evaluate(expect, report) {
  if (expect === "success") {
    return report.requests > 0 && report.success === report.requests && report.errors === 0;
  }
  if (expect === "error") {
    return report.requests > 0 && report.errors > 0;
  }
  if (expect === "mixed") {
    return report.requests > 0 && report.success > 0 && report.errors > 0;
  }
  if (expect === "recovered") {
    return report.requests > 0 && report.success > 0;
  }
  if (expect === "any") {
    return report.requests > 0;
  }
  throw new Error(`unknown expectation: ${expect}`);
}

function safeTail(file, lines = 40) {
  if (!fs.existsSync(file)) {
    return "";
  }
  return fs
    .readFileSync(file, "utf8")
    .split(/\r?\n/)
    .slice(-lines)
    .join("\n")
    .replace(/(authorization|x-api-key|api[_-]?key|bearer)[^,\n]*/gi, "$1=<redacted>");
}

function sampleProcess(pid) {
  const rss = execFileSyncText("ps", ["-o", "rss=", "-p", String(pid)])
    .trim()
    .split(/\s+/)[0];
  const vsz = execFileSyncText("ps", ["-o", "vsz=", "-p", String(pid)])
    .trim()
    .split(/\s+/)[0];
  const cpu = execFileSyncText("ps", ["-o", "%cpu=", "-p", String(pid)])
    .trim()
    .split(/\s+/)[0];
  const lsof = execFileSyncText("lsof", ["-p", String(pid)]);
  return {
    rssBytes: Number(rss || 0) * 1024,
    vszBytes: Number(vsz || 0) * 1024,
    cpuPercent: Number(cpu || 0),
    fdCount: Math.max(0, lsof.split(/\r?\n/).length - 1),
  };
}

function execFileSyncText(command, args) {
  try {
    return execFileSync(command, args, { encoding: "utf8", maxBuffer: 1024 * 1024 });
  } catch {
    return "";
  }
}

async function runWithService(ctx, fakeScenario, fakeOptions, callback) {
  let fake = null;
  let proxy = null;
  try {
    fake = await startFake(ctx, fakeScenario, fakeOptions);
    proxy = await startProxy(ctx, fake.port, {});
    return await callback({ fake, proxy });
  } finally {
    if (proxy) {
      await terminate(proxy.child, `proxy ${proxy.database}`).catch(() => {});
      ctx.children = ctx.children.filter((entry) => entry.child !== proxy.child);
    }
    if (fake) {
      await terminate(fake.child, `fake ${fake.scenario}`).catch(() => {});
      ctx.children = ctx.children.filter((entry) => entry.child !== fake.child);
    }
  }
}

async function runL3(ctx) {
  await runWithService(ctx, "normal-stream", { delayMs: 0 }, async ({ proxy }) => {
    await runLoad(ctx, proxy, {
      id: "l3_normal_c1_r5",
      requests: 5,
      concurrency: 1,
      expect: "success",
    });
    await runLoad(ctx, proxy, {
      id: "l3_normal_c5_r20",
      requests: 20,
      concurrency: 5,
      expect: "success",
    });
    await runLoad(ctx, proxy, {
      id: "l3_normal_c10_r50",
      requests: 50,
      concurrency: 10,
      expect: "success",
    });
    await runLoad(ctx, proxy, {
      id: "l3_spike_c40_r100",
      requests: 100,
      concurrency: 40,
      expect: "success",
    });
    await runLoad(ctx, proxy, {
      id: "l3_recovery_after_spike_c3_r10",
      requests: 10,
      concurrency: 3,
      expect: "success",
    });
  });

  await runWithService(
    ctx,
    "recovery-after-burst",
    { delayMs: 0, recoverAfter: 12 },
    async ({ proxy }) => {
      await runLoad(ctx, proxy, {
        id: "l3_recovery_after_error_burst_c12_r40",
        requests: 40,
        concurrency: 12,
        expect: "recovered",
        timeoutSecs: 30,
      });
      await sleep(1_500);
      await runLoad(ctx, proxy, {
        id: "l3_post_error_recovery_normal_c3_r12",
        requests: 12,
        concurrency: 3,
        expect: "success",
      });
    },
  );

  let savedFake;
  let savedProxy;
  await runWithService(ctx, "invalid-tool-format", { delayMs: 0 }, async ({ fake, proxy }) => {
    savedFake = fake;
    savedProxy = proxy;
    await runLoad(ctx, proxy, {
      id: "l3_invalid_tool_burst_c20_r40",
      requests: 40,
      concurrency: 20,
      expect: "error",
      timeoutSecs: 30,
    });
    savedFake = await restartFake(ctx, fake, "normal-stream", { delayMs: 0 });
    await runLoad(ctx, proxy, {
      id: "l3_invalid_tool_recovery_normal_c3_r12",
      requests: 12,
      concurrency: 3,
      expect: "success",
      timeoutSecs: 30,
    });
  }).finally(async () => {
    if (savedFake) {
      await terminate(savedFake.child, `fake ${savedFake.scenario}`).catch(() => {});
      ctx.children = ctx.children.filter((entry) => entry.child !== savedFake.child);
    }
    void savedProxy;
  });
}

async function runL4(ctx) {
  await runWithService(
    ctx,
    "long-stream",
    { delayMs: 500, streamChunks: 40, streamChunkDelayMs: 100 },
    async ({ fake, proxy }) => {
      const reportPromise = runLoad(ctx, proxy, {
        id: "l4_proxy_restart_during_long_stream",
        requests: 80,
        concurrency: 8,
        durationSecs: 10,
        clientScenario: "long-stream",
        expect: "any",
        timeoutSecs: 45,
        processTimeoutSecs: 60,
      });
      await sleep(2_000);
      proxy = await restartProxy(ctx, proxy, fake.port);
      await reportPromise;
      fake = await restartFake(ctx, fake, "normal-stream", { delayMs: 0 });
      await sleep(1_500);
      await runLoad(ctx, proxy, {
        id: "l4_proxy_restart_recovery_normal_c3_r12",
        requests: 12,
        concurrency: 3,
        expect: "success",
        timeoutSecs: 30,
      });
    },
  );

  for (const [scenario, label] of [
    ["rate-limit429", "rate_limit"],
    ["server-error500", "server_error"],
    ["invalid-tool-format", "invalid_tool"],
  ]) {
    let activeFake;
    await runWithService(ctx, scenario, { delayMs: 0 }, async ({ fake, proxy }) => {
      activeFake = fake;
      await runLoad(ctx, proxy, {
        id: `l4_${label}_burst_c20_r40`,
        requests: 40,
        concurrency: 20,
        expect: "error",
        timeoutSecs: 30,
      });
      activeFake = await restartFake(ctx, fake, "normal-stream", { delayMs: 0 });
      await sleep(1_500);
      await runLoad(ctx, proxy, {
        id: `l4_${label}_recovery_normal_c3_r12`,
        requests: 12,
        concurrency: 3,
        expect: "success",
        timeoutSecs: 30,
      });
    }).finally(async () => {
      if (activeFake) {
        await terminate(activeFake.child, `fake ${activeFake.scenario}`).catch(() => {});
        ctx.children = ctx.children.filter((entry) => entry.child !== activeFake.child);
      }
    });
  }

  await runWithService(ctx, "client-drop", { delayMs: 0 }, async ({ proxy }) => {
    await runLoad(ctx, proxy, {
      id: "l4_client_drop_c20_r40",
      requests: 40,
      concurrency: 20,
      clientScenario: "client-drop",
      expect: "error",
      timeoutSecs: 30,
    });
    await runLoad(ctx, proxy, {
      id: "l4_client_drop_recovery_normal_c3_r12",
      requests: 12,
      concurrency: 3,
      expect: "success",
      timeoutSecs: 30,
    });
  });

  await runWithService(
    ctx,
    "mixed-chaos",
    { delayMs: 300, streamChunks: 16, streamChunkDelayMs: 80 },
    async ({ fake, proxy }) => {
      await runLoad(ctx, proxy, {
        id: "l4_mixed_chaos_c24_r96",
        requests: 96,
        concurrency: 24,
        clientScenario: "mixed-chaos",
        expect: "mixed",
        timeoutSecs: 90,
        processTimeoutSecs: 120,
      });
      await restartFake(ctx, fake, "normal-stream", { delayMs: 0 });
      await sleep(1_500);
      await runLoad(ctx, proxy, {
        id: "l4_mixed_chaos_recovery_normal_c3_r12",
        requests: 12,
        concurrency: 3,
        expect: "success",
        timeoutSecs: 30,
      });
    },
  );
}

async function runL5(ctx, durationSecs, idleCooldownSecs) {
  await runWithService(
    ctx,
    "long-stream",
    { delayMs: 250, streamChunks: 24, streamChunkDelayMs: 100 },
    async ({ fake, proxy }) => {
      // Establish a post-startup allocator baseline before the sustained run.
      // Measuring RSS immediately after process startup makes normal connection
      // pools and serialization buffers look like a leak after the first load.
      await runLoad(ctx, proxy, {
        id: "l5_warmup_baseline_c3_r12",
        requests: 12,
        concurrency: 3,
        expect: "success",
        timeoutSecs: 30,
      });
      const startSample = sampleProcess(proxy.child.pid);
      await runLoad(ctx, proxy, {
        id: `l5_long_stream_soak_${durationSecs}s_c20`,
        requests: 100_000,
        concurrency: 20,
        durationSecs,
        clientScenario: "long-stream",
        expect: "success",
        timeoutSecs: Math.max(120, durationSecs + 60),
        processTimeoutSecs: durationSecs + 120,
      });
      const idleSamples = [];
      const idleSampleCount = Math.max(4, Math.min(12, Math.ceil(idleCooldownSecs / 10)));
      const idleSampleIntervalMs = Math.max(
        1_000,
        Math.floor((idleCooldownSecs * 1_000) / idleSampleCount),
      );
      for (let index = 0; index < idleSampleCount; index += 1) {
        await sleep(idleSampleIntervalMs);
        idleSamples.push(sampleProcess(proxy.child.pid));
      }
      const idleSample = idleSamples.at(-1) || sampleProcess(proxy.child.pid);
      const idleFirstSample = idleSamples[0] || idleSample;
      const idleRssDeltaBytes = idleSample.rssBytes - idleFirstSample.rssBytes;
      const idleVszDeltaBytes = idleSample.vszBytes - idleFirstSample.vszBytes;
      const idleTail = idleSamples.slice(-3);
      const idleTailRssValues = idleTail.map((sample) => sample.rssBytes);
      const idleTailRssMax = Math.max(...idleTailRssValues);
      const idleTailRssMin = Math.min(...idleTailRssValues);
      const idleTailLargeIncreases = idleTail
        .slice(1)
        .filter((sample, index) => (
          sample.rssBytes > idleTail[index].rssBytes + 8 * 1024 * 1024
        )).length;
      ctx.extra.soak = {
        durationSecs,
        idleCooldownSecs,
        startSample,
        idleSample,
        idleSamples,
        idleRssDeltaBytes,
        idleVszDeltaBytes,
        idleRssSettled:
          idleTail.length >= 3 &&
          idleTailLargeIncreases === 0 &&
          idleTailRssMax <= idleFirstSample.rssBytes + 16 * 1024 * 1024 &&
          idleSample.rssBytes <= idleFirstSample.rssBytes + 8 * 1024 * 1024,
        idleTailLargeIncreases,
        idleTailRssRangeBytes: idleTailRssMax - idleTailRssMin,
        rssReturnedWithin32MiB:
          idleSample.rssBytes <= Math.max(startSample.rssBytes + 32 * 1024 * 1024, Math.ceil(startSample.rssBytes * 1.2)),
        fdReturnedWithin5: idleSample.fdCount <= startSample.fdCount + 5,
      };
      await restartFake(ctx, fake, "normal-stream", { delayMs: 0 });
      await runLoad(ctx, proxy, {
        id: "l5_post_soak_recovery_normal_c3_r12",
        requests: 12,
        concurrency: 3,
        expect: "success",
        timeoutSecs: 30,
      });
    },
  );
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const tier = String(args.tier || "l3");
  if (!["l3", "l4", "l5"].includes(tier)) {
    throw new Error(`unsupported --tier ${tier}`);
  }
  const requiredDatabases = requiredServiceCount(tier);
  const productBinary = DEFAULT_PRODUCT_BINARY;
  const loadtestBinary = requirePath(
    "loadtest-binary",
    args["loadtest-binary"] || process.env.KIRO_LOADTEST_BINARY,
  );
  const pgUrlTemplate = String(
    args["postgres-url-template"] || process.env.KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE || "",
  );
  const postgresDatabases = parseDatabaseList(
    args["postgres-databases"] || process.env.KIRO_LOAD_CHAOS_POSTGRES_DATABASES || "",
    requiredDatabases,
  );
  const postgresTarget = validatePostgresTemplate(pgUrlTemplate, postgresDatabases);
  const redisUrl = String(args["redis-url"] || process.env.KIRO_LOAD_CHAOS_REDIS_URL || "");
  const redisPrefix = String(
    args["redis-prefix"] || process.env.KIRO_LOAD_CHAOS_REDIS_PREFIX || "",
  );
  const redisTarget = validateRedisInput(redisUrl, redisPrefix);
  const validateOnly = args["validate-only"] === true || process.env.KIRO_LOAD_CHAOS_VALIDATE_ONLY === "1";
  const summaryPath = args.summary ? path.resolve(String(args.summary)) : null;
  const runId = `${tier}_${Date.now().toString(36)}_${process.pid}`;
  if (validateOnly) {
    process.stdout.write(`${JSON.stringify({
      result: "validate_only",
      tier,
      requiredDatabaseCount: requiredDatabases,
      postgresDatabaseCount: postgresDatabases.length,
      postgresHost: postgresTarget.host,
      postgresPort: postgresTarget.port,
      redisHost: redisTarget.redis.hostname,
      redisPort: redisTarget.port,
      redisDatabase: redisTarget.database,
      dockerUsed: false,
      cargoUsed: false,
      protected9022ProbeSkipped: true,
      createsPostgresDatabase: false,
      dropsPostgresDatabase: false,
      flushesRedisDatabase: false,
      inheritedProcessEnvironment: false,
    }, null, 2)}\n`);
    return;
  }
  const runtimeRoot = path.join(ARTIFACT_ROOT, "runtime");
  fs.mkdirSync(runtimeRoot, { recursive: true });
  const root = fs.mkdtempSync(path.join(runtimeRoot, `kiro-${tier}-load-chaos-`));
  const logsDir = path.join(root, "logs");
  const reportsDir = path.join(root, "reports");
  fs.mkdirSync(logsDir, { recursive: true });
  fs.mkdirSync(reportsDir, { recursive: true });
  const ctx = {
    tier,
    runId,
    root,
    logsDir,
    reportsDir,
    productBinary,
    loadtestBinary,
    productSha256: sha256File(productBinary),
    loadtestSha256: sha256File(loadtestBinary),
    pgUrlTemplate,
    postgresDatabases,
    redisUrl,
    redisTarget,
    redisPrefix,
    proxyCounter: 0,
    sequence: 0,
    children: [],
    usedDatabases: [],
    redisPrefixes: [],
    results: [],
    extra: {},
  };
  let cleanupError = null;
  let passed = false;
  try {
    if (tier === "l3") {
      await runL3(ctx);
    } else if (tier === "l4") {
      await runL4(ctx);
    } else {
      const durationSecs = Number(args["duration-secs"] || 900);
      const idleCooldownSecs = Number(args["idle-cooldown-secs"] || 60);
      await runL5(ctx, durationSecs, idleCooldownSecs);
    }
    passed = ctx.results.every((result) => result.pass);
    if (ctx.extra.soak) {
      passed =
        passed &&
        ctx.extra.soak.rssReturnedWithin32MiB === true &&
        ctx.extra.soak.idleRssSettled === true &&
        ctx.extra.soak.fdReturnedWithin5 === true;
    }
  } finally {
    for (const entry of [...ctx.children].reverse()) {
      await terminate(entry.child, entry.label).catch((error) => {
        cleanupError = cleanupError || error;
      });
    }
    for (const prefix of ctx.redisPrefixes) {
      const deleted = await cleanupRedis(ctx.redisTarget, prefix).catch(() => null);
      ctx.extra.redisCleanup = ctx.extra.redisCleanup || [];
      ctx.extra.redisCleanup.push({ prefix, deleted });
    }
    const logHashes = {};
    for (const file of fs.existsSync(logsDir) ? fs.readdirSync(logsDir) : []) {
      const full = path.join(logsDir, file);
      if (fs.statSync(full).isFile()) {
        logHashes[file] = sha256File(full);
      }
    }
    ctx.extra.logHashes = logHashes;
    const summary = {
      tier,
      runId,
      startedAt: new Date().toISOString(),
      productBinary,
      loadtestBinary,
      productSha256: ctx.productSha256,
      loadtestSha256: ctx.loadtestSha256,
      artifactRoot: ARTIFACT_ROOT,
      postgresDatabaseCount: ctx.postgresDatabases.length,
      usedDatabases: ctx.usedDatabases,
      postgresUrlTemplateRedacted: redactUrl(ctx.pgUrlTemplate),
      redisUrl: ctx.redisUrl,
      redisDatabase: ctx.redisTarget.database,
      redisPrefix: ctx.redisPrefix,
      resultCount: ctx.results.length,
      passed,
      cleanupError: cleanupError ? String(cleanupError.message || cleanupError) : null,
      results: ctx.results,
      extra: ctx.extra,
    };
    if (args["keep-raw"]) {
      summary.rawRoot = root;
    }
    const summaryText = `${JSON.stringify(summary, null, 2)}\n`;
    if (summaryPath) {
      fs.mkdirSync(path.dirname(summaryPath), { recursive: true });
      fs.writeFileSync(summaryPath, summaryText);
    }
    if (!args["keep-raw"]) {
      fs.rmSync(root, { recursive: true, force: true });
    }
    process.stdout.write(summaryText);
    if (!passed || cleanupError) {
      process.exitCode = 1;
    }
  }
}

function redactUrl(urlText) {
  try {
    const parsed = new URL(urlText);
    if (parsed.password) {
      parsed.password = "<redacted>";
    }
    return parsed.toString();
  } catch {
    return "<invalid>";
  }
}

main().catch((error) => {
  console.error(error.stack || error.message || String(error));
  process.exit(1);
});
