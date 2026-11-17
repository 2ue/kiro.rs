#!/usr/bin/env node
// Phased scheduler scenarios against a running claude-pool with mock upstreams.
//
// Each phase sets the mock faults, then keeps `concurrency` clients sending for `durationMs`.
// Every second it samples /readyz and records per-second outcomes, so a stall, a burst of
// errors or a slow recovery is visible in the timeline, not only in totals.
//
// Usage: node scheduler-scenarios.mjs --base-url http://127.0.0.1:PORT --api-key KEY
//          --pool-mock http://127.0.0.1:MOCK
//          --scenario NAME [--path /cc/v1/messages] [--out file.json]

import http from "node:http";
import { writeFileSync } from "node:fs";
import { fdLeakProblems, processSample, resourceSummary } from "./lib/process-sample.mjs";

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    if (!argv[i].startsWith("--")) continue;
    const key = argv[i].slice(2).replace(/-([a-z])/g, (_, ch) => ch.toUpperCase());
    const next = argv[i + 1];
    if (!next || next.startsWith("--")) out[key] = "true";
    else {
      out[key] = next;
      i += 1;
    }
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const baseUrl = new URL(args.baseUrl);
const apiKey = args.apiKey;
const poolMock = args.poolMock;
const prefix = args.accountTokenPrefix || "mock-access-70";
const path = args.path || "/cc/v1/messages";
const agent = new http.Agent({ keepAlive: true, maxSockets: 2048 });
// FD classification: the service's own listen port and the mock upstream port.
const samplePorts = { downstreamPort: baseUrl.port, upstreamPort: new URL(poolMock).port };
const sample = () => processSample(args.targetPid, samplePorts);

const QUIET = { errorRate: 0, errorStatus: 500, headerDelayMs: 300, jitterMs: 0, streamChunks: 0,
  chunkDelayMs: 0, failTokens: [], slowTokens: [], midStreamResetRate: 0, hangRate: 0, trickleRate: 0, retryAfter: "1" };

// Every scenario is a list of phases. `pool` patches the mock at the phase start.
const SCENARIOS = {
  normal: [{ name: "normal", concurrency: 6, durationMs: 10000, pool: QUIET }],
  sudden_burst: [
    { name: "baseline", concurrency: 3, durationMs: 6000, pool: QUIET },
    { name: "burst", concurrency: 60, durationMs: 15000 },
    { name: "after", concurrency: 3, durationMs: 8000 },
  ],
  sustained_overload: [
    { name: "overload", concurrency: 60, durationMs: 40000, pool: { ...QUIET, headerDelayMs: 1500 } },
    { name: "after", concurrency: 3, durationMs: 8000, pool: QUIET },
  ],
  burst_with_errors_recovering: [
    { name: "baseline", concurrency: 6, durationMs: 5000, pool: QUIET },
    { name: "burst_errors", concurrency: 40, durationMs: 15000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 0.3, errorStatus: 500 } },
    { name: "recovered", concurrency: 40, durationMs: 10000, pool: QUIET },
    { name: "after", concurrency: 3, durationMs: 6000 },
  ],
  burst_with_short_outage: [
    { name: "baseline", concurrency: 6, durationMs: 5000, pool: QUIET },
    { name: "outage", concurrency: 30, durationMs: 4000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 1, errorStatus: 503 } },
    { name: "recovered", concurrency: 30, durationMs: 10000, pool: QUIET },
    { name: "after", concurrency: 3, durationMs: 6000 },
  ],
  burst_with_long_outage: [
    { name: "baseline", concurrency: 6, durationMs: 5000, pool: QUIET },
    { name: "outage", concurrency: 30, durationMs: 30000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 1, errorStatus: 429 } },
    { name: "recovered", concurrency: 6, durationMs: 15000, pool: QUIET },
  ],
  single_account_failing: [
    { name: "one_bad_account", concurrency: 12, durationMs: 15000, expect: { ok_rate_min: 0.9 }, pool: { ...QUIET, failTokens: [`${prefix}01`], errorStatus: 500 } },
    { name: "after", concurrency: 6, durationMs: 6000, pool: QUIET },
  ],
  risk_control_burst: [
    { name: "suspended_two", concurrency: 12, durationMs: 12000, expect: { ok_rate_min: 0.9 }, pool: { ...QUIET, failTokens: [`${prefix}01`, `${prefix}02`], errorStatus: 403 } },
    { name: "after", concurrency: 6, durationMs: 6000, pool: QUIET },
  ],
  connection_resets: [
    { name: "resets", concurrency: 20, durationMs: 12000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 0.2, errorStatus: "reset" } },
    { name: "after", concurrency: 6, durationMs: 6000, pool: QUIET },
  ],
  long_stream_burst: [
    { name: "long_streams", concurrency: 30, durationMs: 20000, stream: true, pool: { ...QUIET, headerDelayMs: 200, streamChunks: 60, chunkDelayMs: 100 } },
    { name: "after", concurrency: 3, durationMs: 6000, stream: true, pool: QUIET },
  ],
  small_to_large_burst_errors: [
    { name: "small", concurrency: 2, durationMs: 6000, pool: QUIET },
    { name: "large_errors", concurrency: 60, durationMs: 12000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 0.5, errorStatus: 502 } },
    { name: "large_recovered", concurrency: 60, durationMs: 10000, pool: QUIET },
    { name: "small_again", concurrency: 2, durationMs: 6000 },
  ],

  // Output has started when the upstream drops: the client sees overloaded_error (Claude Code
  // retries only that), never a silently cut stream.
  mid_stream_resets: [
    { name: "cut_streams", concurrency: 15, durationMs: 12000, stream: true, expect: { ok_rate_min: 0, require_terminal: false, allowed_errors: ["overloaded_error", "api_error"] }, pool: { ...QUIET, streamChunks: 20, chunkDelayMs: 50, midStreamResetRate: 0.3 } },
    { name: "after", concurrency: 6, durationMs: 5000, stream: true, pool: QUIET },
  ],
  // Named Anthropic failures, each classified on its own.
  tool_mismatch_500: [
    { name: "tool_mismatch", concurrency: 6, durationMs: 5000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 1, errorStatus: "tool_mismatch" } },
    { name: "after", concurrency: 6, durationMs: 4000, pool: QUIET },
  ],
  gateway_timeout_524: [
    { name: "gateway_timeout", concurrency: 6, durationMs: 5000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 1, errorStatus: "gateway_timeout" } },
    { name: "after", concurrency: 6, durationMs: 4000, pool: QUIET },
  ],
  image_count_exceeded: [
    { name: "image_count", concurrency: 6, durationMs: 5000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 1, errorStatus: "image_count" } },
    { name: "after", concurrency: 6, durationMs: 4000, pool: QUIET },
  ],
  // Account problems still cool or disable the account (only upstream-side faults do not).
  quota_account: [
    { name: "quota_5001", concurrency: 12, durationMs: 8000, expect: { ok_rate_min: 0.9 }, pool: { ...QUIET, failTokens: [`${prefix}01`], errorStatus: "quota" } },
    { name: "after", concurrency: 6, durationMs: 5000, pool: QUIET },
  ],
  auth_account: [
    { name: "auth_5001", concurrency: 12, durationMs: 8000, expect: { ok_rate_min: 0.9 }, pool: { ...QUIET, failTokens: [`${prefix}01`], errorStatus: "auth" } },
    { name: "after", concurrency: 6, durationMs: 5000, pool: QUIET },
  ],
  // Combination: one bad account inside the preferred tier, under overload, with 5xx waves.
  bad_account_in_preferred_tier_overload: [
    { name: "overload_bad_5001", concurrency: 40, durationMs: 15000, expect: { ok_rate_min: 0.9 }, pool: { ...QUIET, failTokens: [`${prefix}01`], errorStatus: 500, headerDelayMs: 800 } },
    { name: "plus_5xx_wave", concurrency: 40, durationMs: 10000, expect: { ok_rate_min: 0.9 }, pool: { ...QUIET, failTokens: [`${prefix}01`], errorStatus: 500, errorRate: 0.2, headerDelayMs: 800 } },
    { name: "after", concurrency: 6, durationMs: 6000, pool: QUIET },
  ],
  // Clients that give up before the upstream answers leave client_dropped usage records.
  client_timeouts: [
    { name: "impatient_clients", concurrency: 12, durationMs: 8000, clientTimeoutMs: 800, expect: { ok_rate_min: 0, allowed_statuses: ["client_timeout"] }, pool: { ...QUIET, headerDelayMs: 2500 } },
    { name: "after", concurrency: 6, durationMs: 5000, pool: QUIET },
  ],

  sudden_burst_sustained: [
    { name: "baseline", concurrency: 3, durationMs: 6000, pool: QUIET },
    { name: "burst", concurrency: 80, durationMs: 90000 },
    { name: "after", concurrency: 3, durationMs: 10000 },
  ],
  long_stream_sessions: [
    { name: "sessions", concurrency: 20, durationMs: 600000, rounds: 30, stableSessions: true, stream: true,
      expect: { ok_rate_min: 1, require_terminal: true, require_rounds: 30 },
      pool: { ...QUIET, headerDelayMs: 0, streamChunks: 120, chunkDelayMs: 250 } },
  ],
  burst_errors_then_recover: [
    { name: "baseline", concurrency: 3, durationMs: 6000, pool: QUIET },
    { name: "errors", concurrency: 60, durationMs: 30000, expect: { ok_rate_min: 0 }, pool: { ...QUIET, errorRate: 1, errorStatus: 500 } },
    { name: "recovered", concurrency: 60, durationMs: 20000, expect: { ok_rate_min: 0.9, recovery_within_secs: 10 }, pool: QUIET },
    { name: "after", concurrency: 3, durationMs: 10000 },
  ],
  burst_errors_never_recover: [
    { name: "baseline", concurrency: 3, durationMs: 6000, pool: QUIET },
    { name: "errors", concurrency: 60, durationMs: 120000,
      expect: { ok_rate_min: 0, allowed_errors: ["overloaded_error", "api_error"], error_only: true, bounded_p95: true, max_stall_secs: 0 },
      pool: { ...QUIET, errorRate: 1, errorStatus: 500 } },
    { name: "after_failing", concurrency: 3, durationMs: 20000,
      expect: { ok_rate_min: 0, allowed_errors: ["overloaded_error", "api_error"], error_only: true, bounded_p95: true, max_stall_secs: 0 } },
  ],
  // 账号异质性：同一个池里同时有故障账号、慢账号、龟速涓流账号，健康账号必须扛住流量，
  // 坏账号被冷却/避让后不能长期拖慢整体 p95。10 账号 x 4 槽 = 40 槽。
  heterogeneous_accounts: [
    { name: "baseline", concurrency: 6, durationMs: 6000, pool: QUIET },
    // 7001 持续 500；7002/7003 首字节慢 8 秒；其余健康。健康 7 账号 28 槽承载 c=24。
    { name: "bad_and_slow", concurrency: 24, durationMs: 45000,
      expect: { ok_rate_min: 0.97 },
      pool: { ...QUIET, failTokens: ["mock-access-7001"], errorStatus: 500,
              slowTokens: ["mock-access-7002", "mock-access-7003"], slowExtraMs: 8000 } },
    // 再叠加 7004/7005 变成龟速：每 4 秒一个 chunk 共 12 个 chunk（约 48 秒一条流）。
    { name: "plus_trickle", concurrency: 24, durationMs: 60000, stream: true,
      expect: { ok_rate_min: 0.95, require_terminal: true },
      pool: { ...QUIET, failTokens: ["mock-access-7001"], errorStatus: 500,
              slowTokens: ["mock-access-7002", "mock-access-7003"], slowExtraMs: 8000,
              trickleRate: 0.25, trickleIntervalMs: 4000, trickleChunks: 12 } },
    { name: "after", concurrency: 6, durationMs: 10000, stream: true, pool: QUIET },
  ],
  // 长流占满：30 个并发长流（每条约 60 秒，每 2 秒一个 chunk）把 40 槽占掉 30 个，
  // 同时 20 并发短请求必须仍能在剩余槽位上推进，且长流结束后槽位全部回收。
  long_streams_hold_slots: [
    { name: "baseline", concurrency: 6, durationMs: 6000, pool: QUIET },
    { name: "long_plus_short", concurrency: 50, durationMs: 90000, stream: true,
      expect: { ok_rate_min: 0.97, require_terminal: true },
      pool: { ...QUIET, trickleRate: 0.6, trickleIntervalMs: 2000, trickleChunks: 30 } },
    { name: "after", concurrency: 6, durationMs: 15000, stream: true, pool: QUIET },
  ],
  // 账号在负载中途被禁用再恢复（管理端会怎样在真实场景发生：风控/人工）：用 mock 的
  // failTokens 让 3 个账号同时 403 60 秒，期间其余 7 个账号承载；恢复后 10 个账号都要重新被调度到。
  accounts_suspended_then_restored: [
    { name: "baseline", concurrency: 12, durationMs: 6000, pool: QUIET },
    { name: "three_suspended", concurrency: 20, durationMs: 60000,
      expect: { ok_rate_min: 0.97 },
      pool: { ...QUIET, failTokens: ["mock-access-7001", "mock-access-7002", "mock-access-7003"], errorStatus: 403 } },
    { name: "restored", concurrency: 20, durationMs: 40000, expect: { ok_rate_min: 0.97, require_all_accounts_used: true }, pool: QUIET },
    { name: "after", concurrency: 6, durationMs: 8000, pool: QUIET },
  ],
  // 全部叠加 8 分钟：故障账号 + 慢账号 + 龟速 + 长流 + 中途断流 + 偶发 429/500，c=40。
  everything_at_once: [
    { name: "baseline", concurrency: 6, durationMs: 6000, pool: QUIET },
    { name: "storm", concurrency: 40, durationMs: 480000, stream: true,
      expect: { ok_rate_min: 0.85, require_terminal: false, max_stall_secs: 5,
                allowed_errors: ["api_error", "overloaded_error", "rate_limit_error"] },
      pool: { ...QUIET, errorRate: 0.1, errorStatus: 500,
              failTokens: ["mock-access-7001"], slowTokens: ["mock-access-7002"], slowExtraMs: 6000,
              trickleRate: 0.15, trickleIntervalMs: 3000, trickleChunks: 15,
              midStreamResetRate: 0.05, streamChunks: 20, chunkDelayMs: 50 } },
    { name: "recovered", concurrency: 6, durationMs: 60000, stream: true, pool: QUIET },
  ],
  mixed_chaos_soak: [
    ...Array.from({ length: 9 }, (_, index) => ({
      name: `chaos_${index + 1}`, concurrency: 40, durationMs: 60000, stream: true,
      expect: { ok_rate_min: 0, require_terminal: false, allowed_errors: ["api_error", "overloaded_error", "rate_limit_error"] },
      pool: { ...QUIET, streamChunks: 20, chunkDelayMs: 50, ...[
        { errorRate: 0.3, errorStatus: 500 }, { errorRate: 0.3, errorStatus: 429 },
        { errorRate: 0.2, errorStatus: "reset" }, { midStreamResetRate: 0.3 },
        { hangRate: 0.05 }, { headerDelayMs: 3000 },
      ][index % 6] },
    })),
    { name: "recovered", concurrency: 6, durationMs: 60000, stream: true, pool: QUIET },
  ],

};

for (const phases of Object.values(SCENARIOS)) {
  for (const phase of phases) phase.expect = {
    ok_rate_min: /recover|after/.test(phase.name) ? 0.9 : 0.99,
    max_stall_secs: 2, require_terminal: Boolean(phase.stream), ...phase.expect,
  };
}

async function patchMock(target, patch) {
  if (!target || !patch) return null;
  const response = await fetch(`${target}/__control`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...patch, resetCounters: true }),
    signal: AbortSignal.timeout(5000),
  });
  return response.json();
}

async function mockCounters(target) {
  if (!target) return null;
  try {
    return (await (await fetch(`${target}/__control`, { signal: AbortSignal.timeout(5000) })).json()).counters;
  } catch {
    return null;
  }
}

function requestBody(stream, marker, session) {
  return { model: args.model || "sonnet", max_tokens: 4096, stream,
    metadata: { user_id: `user_scn_account__session_${session || marker}` },
    messages: [{ role: "user", content: `scenario request ${marker}` }] };
}

function sendOne(stream, marker, phase = {}) {
  const requestPath = phase.path || path;
  const body = JSON.stringify(requestBody(stream, marker, phase.session));
  const clientTimeoutMs = phase.clientTimeoutMs || 120000;
  const started = performance.now();
  return new Promise((resolve) => {
    const req = http.request(
      {
        host: baseUrl.hostname,
        port: baseUrl.port,
        path: requestPath,
        method: "POST",
        agent,
        headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body), "x-api-key": apiKey },
      },
      (res) => {
        // Keep the head (error envelopes) and the tail (terminal events) of the body.
        let head = "";
        let tail = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          if (head.length < 4096) head += chunk;
          tail = (tail + chunk).slice(-4096);
        });
        res.on("end", () => {
          const text = head + tail;
          let status = String(res.statusCode);
          let errorType = null;
          if (stream && res.statusCode === 200) {
            const match = text.match(/"type"\s*:\s*"(overloaded_error|api_error|rate_limit_error|[a-z_]+_error)"/);
            if (/event: error|response\.failed|"error"\s*:\s*\{/.test(text)) {
              status = "stream_error";
              errorType = match ? match[1] : "unknown";
            } else if (!/event:\s*message_stop(?:\r?\n|$)/.test(text)) {
              status = "stream_cut";
            }
          } else if (res.statusCode !== 200) {
            const match = head.match(/"type"\s*:\s*"([a-z_]+_error)"/);
            errorType = match ? match[1] : null;
          }
          resolve({
            status,
            errorType,
            ms: performance.now() - started,
            retryAfter: res.headers["retry-after"] || null,
            shouldRetry: res.headers["x-should-retry"] || null,
          });
        });
        res.on("error", () => resolve({ status: stream ? "stream_cut" : "read_error", ms: performance.now() - started }));
      },
    );
    const deadline = setTimeout(() => req.destroy(new Error("client_timeout")), clientTimeoutMs);
    req.once("close", () => clearTimeout(deadline));
    req.on("error", (error) => resolve({ status: error.message === "client_timeout" ? "client_timeout" : "conn_error", ms: performance.now() - started }));
    req.end(body);
  });
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const pct = (list, p) => {
  if (!list.length) return null;
  const sorted = [...list].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor(p / 100 * sorted.length))]);
};

async function runPhase(phase, startedAt, timeline, resources) {
  await patchMock(poolMock, phase.pool);
  const phaseStarted = Date.now();
  const durationMs = phase.durationMs * Number(args.durationScale || 1);
  const end = phaseStarted + durationMs;
  const results = [];
  const rounds = Array(phase.concurrency).fill(0);
  let sampling = true;
  const worker = async (id) => {
    while ((phase.rounds ? rounds[id] < phase.rounds : Date.now() < end)) {
      const round = rounds[id]++;
      const marker = `${phase.name}-${id}-${round}`;
      const result = await sendOne(Boolean(phase.stream), marker, {
        ...phase, session: phase.stableSessions ? `long-session-${id}` : marker,
      });
      result.at = Date.now();
      result.elapsedMs = result.at - phaseStarted;
      results.push(result);
    }
  };
  const sampler = (async () => {
    while (sampling) {
      await sleep(1000);
      let ready;
      try {
        const response = await fetch(`${baseUrl.origin}/readyz`, { signal: AbortSignal.timeout(3000) });
        const data = await response.json();
        ready = { status: response.status, dispatch: data.dispatch };
      } catch { ready = { status: "down" }; }
      const lastSecond = results.filter((result) => result.at > Date.now() - 1000);
      resources.push(sample());
      timeline.push({ t: Math.round((Date.now() - startedAt) / 1000), phase: phase.name,
        ok: lastSecond.filter((result) => result.status === "200").length,
        not_ok: lastSecond.filter((result) => result.status !== "200").length, ready });
    }
  })();
  try {
    await Promise.all(Array.from({ length: phase.concurrency }, (_, id) => worker(id)));
    // Thirty 30-second rounds need at least fifteen minutes. Keep every round and the
    // requested ten-minute minimum rather than silently dropping unfinished sessions.
    if (phase.rounds && Date.now() < end) await sleep(end - Date.now());
  } finally { sampling = false; await sampler; }
  const byStatus = {}, errorTypes = {}, retryAfters = {};
  for (const result of results) {
    byStatus[result.status] = (byStatus[result.status] || 0) + 1;
    if (result.retryAfter) retryAfters[result.retryAfter] = (retryAfters[result.retryAfter] || 0) + 1;
    if (result.status !== "200") {
      const key = `${result.status}:${result.errorType || "-"}`;
      errorTypes[key] = (errorTypes[key] || 0) + 1;
    }
  }
  const expect = { ok_rate_min: /recover|after/.test(phase.name) ? 0.9 : 0.99,
    max_stall_secs: 2, require_terminal: Boolean(phase.stream), ...phase.expect };
  const samples = timeline.filter((sample) => sample.phase === phase.name);
  const stalls = samples.filter((sample) => sample.ready.dispatch?.progressing === false).length;
  const problems = [];
  const rate = results.length ? (byStatus["200"] || 0) / results.length : 0;
  if (!results.length || rate < expect.ok_rate_min) problems.push(`ok rate ${rate.toFixed(3)} < ${expect.ok_rate_min}`);
  if (stalls > expect.max_stall_secs) problems.push(`stall ${stalls}s > ${expect.max_stall_secs}s`);
  if (samples.some((sample) => sample.ready.status !== 200)) problems.push("/readyz did not stay 200");
  if (byStatus.conn_error || byStatus.read_error || (byStatus.client_timeout && !expect.allowed_statuses?.includes("client_timeout"))) problems.push("unexpected client transport failure");
  if (byStatus.stream_cut) problems.push("stream_cut without terminal event or error envelope");
  if (expect.require_terminal && (byStatus.stream_cut || byStatus.stream_error)) problems.push("stream missing message_stop");
  if (expect.allowed_errors && results.some((r) => r.status !== "200" && !expect.allowed_errors.includes(r.errorType))) problems.push("unexpected error envelope or connection failure");
  if (expect.allowed_statuses && results.some((r) => !expect.allowed_statuses.includes(r.status))) problems.push("unexpected status");
  if (expect.error_only && results.some((r) => r.status === "200")) problems.push("unexpected success during complete outage");
  if (expect.require_rounds && rounds.some((round) => round !== expect.require_rounds)) problems.push("incomplete session rounds");
  if (expect.require_all_accounts_used) {
    const used = Object.keys((await mockCounters(poolMock))?.perToken || {}).length;
    const total = Number(args.accounts || 10);
    if (used < total) problems.push(`only ${used}/${total} accounts were dispatched to after restore`);
  }
  const p95 = pct(results.map((r) => r.ms), 95);
  if (expect.bounded_p95 && p95 >= (Number(args.dispatchMaxWaitSecs || 60) + 10) * 1000) problems.push("p95 exceeds dispatch wait + 10s");
  const recovery = results.filter((r) => r.elapsedMs <= (expect.recovery_within_secs || 0) * 1000);
  if (expect.recovery_within_secs && (!recovery.length || recovery.filter((r) => r.status === "200").length / recovery.length <= 0.9)) problems.push("first 10s recovery ok rate <= 90%");
  return { phase: phase.name, concurrency: phase.concurrency, durationMs: Date.now() - phaseStarted,
    expect, total: results.length, byStatus, errorTypes, okRate: Math.round(rate * 1000) / 10,
    okP50: pct(results.filter((r) => r.status === "200").map((r) => r.ms), 50),
    okP95: pct(results.filter((r) => r.status === "200").map((r) => r.ms), 95), p95,
    rejectP50: pct(results.filter((r) => ["429", "529"].includes(r.status)).map((r) => r.ms), 50),
    retryAfters, stallSecs: stalls, rounds: phase.rounds ? rounds : undefined,
    verdict: problems.length ? "FAIL" : "PASS", problems, ...(await upstreamSummary()) };
}

// Mock counters plus, from the recorded conversation identities, how often a call that followed
// a failed one reused the failed call's conversationId (a retry must use a fresh one).
async function upstreamSummary() {
  const upstream = await mockCounters(poolMock);
  if (!upstream) return { upstream };
  const conversations = upstream.conversations || [];
  delete upstream.conversations;
  const failedIds = new Set(conversations.filter((call) => call.outcome === "failed").map((call) => call.conversationId));
  const reused = conversations.filter((call, index) => {
    if (!call.conversationId) return false;
    const earlier = conversations.slice(0, index);
    return earlier.some((prior) => prior.conversationId === call.conversationId);
  }).length;
  const retryIdentity = failedIds.size ? { failedCalls: failedIds.size, callsReusingAnEarlierConversationId: reused } : null;
  return { upstream, retryIdentity };
}

async function main() {
  const phases = SCENARIOS[args.scenario];
  if (!phases) throw new Error(`unknown scenario ${args.scenario}; known: ${Object.keys(SCENARIOS).join(", ")}`);
  const startedAt = Date.now();
  const timeline = [], results = [], resources = [sample()];
  for (const phase of phases) results.push(await runPhase(phase, startedAt, timeline, resources));
  // Observe release/drain after the last clients finish, including canceled upstream sockets.
  await patchMock(poolMock, QUIET);
  // Close this script's own keep-alive sockets first: idle downstream connections are the
  // client's, not the service's, and must not count as service FDs in the recovery check.
  agent.destroy();
  // Soak scenarios judge FD recovery, so wait past the upstream client's idle-pool timeout
  // (reqwest default 90 s) before the final sample; other scenarios only need a short drain.
  const fdChecked = ["long_stream_sessions", "mixed_chaos_soak", "everything_at_once", "long_streams_hold_slots", "heterogeneous_accounts"].includes(args.scenario);
  await sleep(Number(args.settleSecs || (fdChecked ? 100 : 5)) * 1000);
  resources.push(sample());
  const resource = resourceSummary(resources);
  const problems = results.flatMap((result) => result.problems.map((problem) => `${result.phase}: ${problem}`));
  if (fdChecked) {
    if (!resource || !resource.end.alive) problems.push("resource samples unavailable or process exited");
    else {
      if (!(resource.end.rssMb <= resource.peak.rssMb * 0.8 || resource.end.rssMb <= resource.start.rssMb + 150)) problems.push("RSS did not recover");
      // Pooled idle sockets (reqwest 90 s, sqlx 10 min) outlive the settle window and are bounded
      // by caps, so FDs are judged per kind instead of as a total (see lib/process-sample.mjs).
      problems.push(...fdLeakProblems(resource.start, resource.end, {
        upstreamCap: Number(args.upstreamSocketCap || 0), dbCap: Number(args.dbSocketCap || 0) }));
    }
  }
  if (resources.some((sample) => sample && !sample.alive)) problems.push("service process exited");
  const stalls = timeline.filter((sample) => sample.ready.dispatch?.progressing === false);
  const summary = { scenario: args.scenario, phases: results, stalledSeconds: stalls.length,
    timeline, resources: resource, problems, verdict: problems.length ? "FAIL" : "PASS" };
  if (args.out) writeFileSync(args.out, JSON.stringify(summary, null, 2));
  for (const result of results) console.log(`${args.scenario}/${result.phase} c=${result.concurrency} total=${result.total} ok%=${result.okRate} status=${JSON.stringify(result.byStatus)} p95=${result.p95} stall=${result.stallSecs} verdict=${result.verdict} ${result.problems.join("; ")}`);
  console.log(`verdict: ${summary.verdict}`);
  process.exitCode = summary.verdict === "PASS" ? 0 : 1;
}

main().catch((error) => {
  console.error(error);
  agent.destroy();
  process.exitCode = 1;
});
