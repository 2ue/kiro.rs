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

function sendOne(stream, marker) {
  const body = JSON.stringify({
    model: args.model || "sonnet",
    max_tokens: 64,
    stream,
    metadata: { user_id: `user_scn_account__session_${marker}` },
    messages: [{ role: "user", content: `scenario request ${marker}` }],
  });
  const started = performance.now();
  return new Promise((resolve) => {
    const req = http.request(
      {
        host: baseUrl.hostname,
        port: baseUrl.port,
        path,
        method: "POST",
        agent,
        headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body), "x-api-key": apiKey },
      },
      (res) => {
        let text = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          if (text.length < 4096) text += chunk;
        });
        res.on("end", () => {
          const streamError = stream && res.statusCode === 200 && /event: error/.test(text);
          resolve({
            status: streamError ? "stream_error" : String(res.statusCode),
            ms: performance.now() - started,
            retryAfter: res.headers["retry-after"] || null,
          });
        });
        res.on("error", () => resolve({ status: "read_error", ms: performance.now() - started }));
      },
    );
    req.setTimeout(120000, () => req.destroy(new Error("client_timeout")));
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
      const result = await sendOne(Boolean(phase.stream), `${phase.name}-${id}-${seq++}`);
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
  return {
    phase: phase.name,
    concurrency: phase.concurrency,
    total: results.length,
    byStatus,
    okRate: results.length ? Math.round((1000 * (byStatus["200"] || 0)) / results.length) / 10 : null,
    okP50: pct(okLatencies, 50),
    okP95: pct(okLatencies, 95),
    rejectP50: pct(rejectLatencies, 50),
    retryAfters,
    upstream: await mockCounters(kiroMock),
    external: await mockCounters(externalMock),
  };
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
    console.log(
      `${args.scenario}/${result.phase} c=${result.concurrency} total=${result.total} ok%=${result.okRate} status=${JSON.stringify(result.byStatus)} okP50=${result.okP50} okP95=${result.okP95} rejectP50=${result.rejectP50} retryAfter=${JSON.stringify(result.retryAfters)} upstream=${JSON.stringify(result.upstream && { req: result.upstream.requests, ok: result.upstream.ok, failed: result.upstream.failed, maxInFlight: result.upstream.maxInFlight })}${result.external ? ` external=${JSON.stringify({ req: result.external.requests, ok: result.external.ok, failed: result.external.failed })}` : ""}`,
    );
  }
  console.log(`${args.scenario}: stalled seconds=${stalls.length}`);
  agent.destroy();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
