#!/usr/bin/env node
// Phased scheduler scenarios against a running kiro-rs with mock upstreams.
//
// Each phase sets the mock faults, then keeps `concurrency` clients sending for `durationMs`.
// Every second it samples /readyz and records per-second outcomes, so a stall, a burst of
// errors or a slow recovery is visible in the timeline, not only in totals.
//
// Usage: node scheduler-scenarios.mjs --base-url http://127.0.0.1:PORT --api-key KEY
//          --kiro-mock http://127.0.0.1:MOCK --external-mock http://127.0.0.1:EXT
//          --scenario NAME [--path /cc/v1/messages] [--out file.json]

import http from "node:http";

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
const kiroMock = args.kiroMock;
const externalMock = args.externalMock;
const path = args.path || "/cc/v1/messages";
const agent = new http.Agent({ keepAlive: true, maxSockets: 2048 });

const QUIET = { errorRate: 0, headerDelayMs: 300, streamChunks: 0, chunkDelayMs: 0, failTokens: [] };

// Every scenario is a list of phases. `kiro` / `external` patch the mocks at the phase start.
const SCENARIOS = {
  normal: [{ name: "normal", concurrency: 6, durationMs: 10000, kiro: QUIET }],
  sudden_burst: [
    { name: "baseline", concurrency: 3, durationMs: 6000, kiro: QUIET },
    { name: "burst", concurrency: 60, durationMs: 15000 },
    { name: "after", concurrency: 3, durationMs: 8000 },
  ],
  sustained_overload: [
    { name: "overload", concurrency: 60, durationMs: 40000, kiro: { ...QUIET, headerDelayMs: 1500 } },
    { name: "after", concurrency: 3, durationMs: 8000, kiro: QUIET },
  ],
  burst_with_errors_recovering: [
    { name: "baseline", concurrency: 6, durationMs: 5000, kiro: QUIET },
    { name: "burst_errors", concurrency: 40, durationMs: 15000, kiro: { ...QUIET, errorRate: 0.3, errorStatus: 500 } },
    { name: "recovered", concurrency: 40, durationMs: 10000, kiro: QUIET },
    { name: "after", concurrency: 3, durationMs: 6000 },
  ],
  burst_with_short_outage: [
    { name: "baseline", concurrency: 6, durationMs: 5000, kiro: QUIET },
    { name: "outage", concurrency: 30, durationMs: 4000, kiro: { ...QUIET, errorRate: 1, errorStatus: 503 } },
    { name: "recovered", concurrency: 30, durationMs: 10000, kiro: QUIET },
    { name: "after", concurrency: 3, durationMs: 6000 },
  ],
  burst_with_long_outage: [
    { name: "baseline", concurrency: 6, durationMs: 5000, kiro: QUIET },
    { name: "outage", concurrency: 30, durationMs: 30000, kiro: { ...QUIET, errorRate: 1, errorStatus: 429 } },
    { name: "recovered", concurrency: 6, durationMs: 15000, kiro: QUIET },
  ],
  single_account_failing: [
    { name: "one_bad_account", concurrency: 12, durationMs: 15000, kiro: { ...QUIET, failTokens: ["mock-access-5001"], errorStatus: 500 } },
    { name: "after", concurrency: 6, durationMs: 6000, kiro: QUIET },
  ],
  risk_control_burst: [
    { name: "suspended_two", concurrency: 12, durationMs: 12000, kiro: { ...QUIET, failTokens: ["mock-access-5001", "mock-access-5002"], errorStatus: 403 } },
    { name: "after", concurrency: 6, durationMs: 6000, kiro: QUIET },
  ],
  connection_resets: [
    { name: "resets", concurrency: 20, durationMs: 12000, kiro: { ...QUIET, errorRate: 0.2, errorStatus: "reset" } },
    { name: "after", concurrency: 6, durationMs: 6000, kiro: QUIET },
  ],
  long_stream_burst: [
    { name: "long_streams", concurrency: 30, durationMs: 20000, stream: true, kiro: { ...QUIET, headerDelayMs: 200, streamChunks: 60, chunkDelayMs: 100 } },
    { name: "after", concurrency: 3, durationMs: 6000, stream: true, kiro: QUIET },
  ],
  small_to_large_burst_errors: [
    { name: "small", concurrency: 2, durationMs: 6000, kiro: QUIET },
    { name: "large_errors", concurrency: 60, durationMs: 12000, kiro: { ...QUIET, errorRate: 0.5, errorStatus: 502 } },
    { name: "large_recovered", concurrency: 60, durationMs: 10000, kiro: QUIET },
    { name: "small_again", concurrency: 2, durationMs: 6000 },
  ],

  // Protocols: Anthropic and both OpenAI interfaces, streaming and not, under errors and overload.
  protocols_with_errors: [
    { name: "messages_stream", concurrency: 8, durationMs: 6000, stream: true, kiro: { ...QUIET, errorRate: 0.3, errorStatus: 500 } },
    { name: "chat", concurrency: 8, durationMs: 6000, protocol: "chat", path: "/cc/v1/chat/completions" },
    { name: "chat_stream", concurrency: 8, durationMs: 6000, protocol: "chat", path: "/cc/v1/chat/completions", stream: true },
    { name: "responses", concurrency: 8, durationMs: 6000, protocol: "responses", path: "/cc/v1/responses" },
    { name: "responses_stream", concurrency: 8, durationMs: 6000, protocol: "responses", path: "/cc/v1/responses", stream: true },
  ],
  openai_overload: [
    { name: "chat_overload", concurrency: 60, durationMs: 10000, protocol: "chat", path: "/v1/chat/completions", kiro: { ...QUIET, headerDelayMs: 1500 } },
    { name: "responses_overload", concurrency: 60, durationMs: 10000, protocol: "responses", path: "/v1/responses" },
    { name: "after", concurrency: 3, durationMs: 5000, protocol: "chat", path: "/v1/chat/completions", kiro: QUIET },
  ],
  // Output has started when the upstream drops: the client sees overloaded_error (Claude Code
  // retries only that), never a silently cut stream.
  mid_stream_resets: [
    { name: "cut_streams", concurrency: 15, durationMs: 12000, stream: true, kiro: { ...QUIET, streamChunks: 20, chunkDelayMs: 50, midStreamResetRate: 0.3 } },
    { name: "after", concurrency: 6, durationMs: 5000, stream: true, kiro: QUIET },
  ],
  // Named Kiro failures, each classified on its own.
  tool_mismatch_500: [
    { name: "tool_mismatch", concurrency: 6, durationMs: 5000, kiro: { ...QUIET, errorRate: 1, errorStatus: "tool_mismatch" } },
    { name: "after", concurrency: 6, durationMs: 4000, kiro: QUIET },
  ],
  gateway_timeout_524: [
    { name: "gateway_timeout", concurrency: 6, durationMs: 5000, kiro: { ...QUIET, errorRate: 1, errorStatus: "gateway_timeout" } },
    { name: "after", concurrency: 6, durationMs: 4000, kiro: QUIET },
  ],
  image_count_exceeded: [
    { name: "image_count", concurrency: 6, durationMs: 5000, kiro: { ...QUIET, errorRate: 1, errorStatus: "image_count" } },
    { name: "after", concurrency: 6, durationMs: 4000, kiro: QUIET },
  ],
  // Account problems still cool or disable the account (only upstream-side faults do not).
  quota_account: [
    { name: "quota_5001", concurrency: 12, durationMs: 8000, kiro: { ...QUIET, failTokens: ["mock-access-5001"], errorStatus: "quota" } },
    { name: "after", concurrency: 6, durationMs: 5000, kiro: QUIET },
  ],
  auth_account: [
    { name: "auth_5001", concurrency: 12, durationMs: 8000, kiro: { ...QUIET, failTokens: ["mock-access-5001"], errorStatus: "auth" } },
    { name: "after", concurrency: 6, durationMs: 5000, kiro: QUIET },
  ],
  // Combination: one bad account inside the preferred tier, under overload, with 5xx waves.
  bad_account_in_preferred_tier_overload: [
    { name: "overload_bad_5001", concurrency: 40, durationMs: 15000, kiro: { ...QUIET, failTokens: ["mock-access-5001"], errorStatus: 500, headerDelayMs: 800 } },
    { name: "plus_5xx_wave", concurrency: 40, durationMs: 10000, kiro: { ...QUIET, failTokens: ["mock-access-5001"], errorStatus: 500, errorRate: 0.2, headerDelayMs: 800 } },
    { name: "after", concurrency: 6, durationMs: 6000, kiro: QUIET },
  ],
  // Clients that give up before the upstream answers leave client_dropped usage records.
  client_timeouts: [
    { name: "impatient_clients", concurrency: 12, durationMs: 8000, clientTimeoutMs: 800, kiro: { ...QUIET, headerDelayMs: 2500 } },
    { name: "after", concurrency: 6, durationMs: 5000, kiro: QUIET },
  ],
  // External pool: takes over when the local pool is full or failing, steps aside when it fails.
  external_overflow: [
    { name: "local_full", concurrency: 40, durationMs: 10000, kiro: { ...QUIET, headerDelayMs: 1500 }, external: { errorRate: 0, headerDelayMs: 200 } },
    { name: "local_failing", concurrency: 12, durationMs: 8000, kiro: { ...QUIET, errorRate: 1, errorStatus: 503 } },
    { name: "external_failing", concurrency: 40, durationMs: 8000, kiro: { ...QUIET, headerDelayMs: 1500 }, external: { errorRate: 1, errorStatus: 503 } },
    { name: "both_ok", concurrency: 12, durationMs: 6000, kiro: QUIET, external: { errorRate: 0 } },
  ],
  external_on_524: [
    { name: "local_524", concurrency: 6, durationMs: 6000, kiro: { ...QUIET, errorRate: 1, errorStatus: "gateway_timeout" }, external: { errorRate: 0, headerDelayMs: 100 } },
    { name: "after", concurrency: 6, durationMs: 4000, kiro: QUIET },
  ],
};

async function patchMock(target, patch) {
  if (!target || !patch) return null;
  const response = await fetch(`${target}/__control`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...patch, resetCounters: true }),
  });
  return response.json();
}

async function mockCounters(target) {
  if (!target) return null;
  try {
    return (await (await fetch(`${target}/__control`)).json()).counters;
  } catch {
    return null;
  }
}

// Request body for each protocol the proxy accepts on the phase's path.
function requestBody(protocol, stream, marker) {
  const model = args.model || "sonnet";
  const text = `scenario request ${marker}`;
  if (protocol === "chat") {
    return { model, max_tokens: 64, stream, user: `session_${marker}`, messages: [{ role: "user", content: text }] };
  }
  if (protocol === "responses") {
    return { model, max_output_tokens: 64, stream, input: text };
  }
  return {
    model,
    max_tokens: 64,
    stream,
    metadata: { user_id: `user_scn_account__session_${marker}` },
    messages: [{ role: "user", content: text }],
  };
}

// A stream that ended without its protocol's terminal event was cut.
function streamFinished(protocol, text) {
  if (protocol === "chat") return /data: \[DONE\]/.test(text);
  if (protocol === "responses") return /response\.(completed|incomplete)/.test(text);
  return /message_stop/.test(text);
}

function sendOne(stream, marker, phase = {}) {
  const protocol = phase.protocol || "messages";
  const requestPath = phase.path || path;
  const body = JSON.stringify(requestBody(protocol, stream, marker));
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
            } else if (!streamFinished(protocol, text)) {
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
        res.on("error", () => resolve({ status: "read_error", ms: performance.now() - started }));
      },
    );
    req.setTimeout(clientTimeoutMs, () => req.destroy(new Error("client_timeout")));
    req.on("error", (error) => resolve({ status: error.message === "client_timeout" ? "client_timeout" : "conn_error", ms: performance.now() - started }));
    req.end(body);
  });
}

async function runPhase(phase, startedAt, timeline) {
  if (phase.kiro) await patchMock(kiroMock, phase.kiro);
  if (phase.external) await patchMock(externalMock, phase.external);
  const end = Date.now() + phase.durationMs;
  const results = [];
  let seq = 0;
  const worker = async (id) => {
    while (Date.now() < end) {
      const result = await sendOne(Boolean(phase.stream), `${phase.name}-${id}-${seq++}`, phase);
      result.at = Date.now();
      results.push(result);
    }
  };
  const sampler = (async () => {
    while (Date.now() < end) {
      await new Promise((resolve) => setTimeout(resolve, 1000));
      let ready = null;
      try {
        const response = await fetch(`${baseUrl.origin}/readyz`);
        const data = await response.json();
        ready = { status: response.status, queued: data.dispatch?.queuedRequests, progressing: data.dispatch?.progressing };
      } catch {
        ready = { status: "down" };
      }
      const lastSecond = results.filter((result) => result.at > Date.now() - 1000);
      timeline.push({
        t: Math.round((Date.now() - startedAt) / 1000),
        phase: phase.name,
        ok: lastSecond.filter((result) => result.status === "200").length,
        not_ok: lastSecond.filter((result) => result.status !== "200").length,
        ready,
      });
    }
  })();
  await Promise.all([...Array.from({ length: phase.concurrency }, (_, id) => worker(id)), sampler]);
  const byStatus = {};
  for (const result of results) byStatus[result.status] = (byStatus[result.status] || 0) + 1;
  const okLatencies = results.filter((result) => result.status === "200").map((result) => result.ms).sort((a, b) => a - b);
  const rejectLatencies = results.filter((result) => result.status === "429").map((result) => result.ms).sort((a, b) => a - b);
  const pct = (list, p) => (list.length ? Math.round(list[Math.min(list.length - 1, Math.floor((p / 100) * list.length))]) : null);
  const retryAfters = {};
  for (const result of results) if (result.retryAfter) retryAfters[result.retryAfter] = (retryAfters[result.retryAfter] || 0) + 1;
  const errorTypes = {};
  for (const result of results) {
    if (result.status === "200") continue;
    const key = `${result.status}:${result.errorType || "-"}${result.shouldRetry ? `:retry=${result.shouldRetry}` : ""}`;
    errorTypes[key] = (errorTypes[key] || 0) + 1;
  }
  return {
    phase: phase.name,
    concurrency: phase.concurrency,
    total: results.length,
    byStatus,
    errorTypes,
    okRate: results.length ? Math.round((1000 * (byStatus["200"] || 0)) / results.length) / 10 : null,
    okP50: pct(okLatencies, 50),
    okP95: pct(okLatencies, 95),
    rejectP50: pct(rejectLatencies, 50),
    retryAfters,
    ...(await upstreamSummary()),
    external: await mockCounters(externalMock),
  };
}

// Mock counters plus, from the recorded conversation identities, how often a call that followed
// a failed one reused the failed call's conversationId (a retry must use a fresh one).
async function upstreamSummary() {
  const upstream = await mockCounters(kiroMock);
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
  const timeline = [];
  const results = [];
  for (const phase of phases) results.push(await runPhase(phase, startedAt, timeline));
  const stalls = timeline.filter((sample) => sample.ready?.status === 503 || sample.ready?.progressing === false);
  const summary = { scenario: args.scenario, phases: results, stalledSeconds: stalls.length, timeline };
  if (args.out) {
    const fs = await import("node:fs");
    fs.writeFileSync(args.out, JSON.stringify(summary, null, 2));
  }
  for (const result of results) {
    const upstream = result.upstream && {
      req: result.upstream.requests,
      ok: result.upstream.ok,
      failed: result.upstream.failed,
      maxInFlight: result.upstream.maxInFlight,
      ...(result.upstream.midStreamResets ? { midStreamResets: result.upstream.midStreamResets } : {}),
      byToken: Object.fromEntries(Object.entries(result.upstream.byToken || {}).map(([k, v]) => [k.replace("mock-access-", ""), v])),
    };
    console.log(
      `${args.scenario}/${result.phase} c=${result.concurrency} total=${result.total} ok%=${result.okRate} status=${JSON.stringify(result.byStatus)}${Object.keys(result.errorTypes).length ? ` errors=${JSON.stringify(result.errorTypes)}` : ""} okP50=${result.okP50} okP95=${result.okP95} rejectP50=${result.rejectP50} retryAfter=${JSON.stringify(result.retryAfters)} upstream=${JSON.stringify(upstream)}${result.external ? ` external=${JSON.stringify({ req: result.external.requests, ok: result.external.ok, failed: result.external.failed })}` : ""}`,
    );
    if (result.retryIdentity) console.log(`  retry identity: ${JSON.stringify(result.retryIdentity)}`);
  }
  console.log(`${args.scenario}: stalled seconds=${stalls.length}`);
  agent.destroy();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
