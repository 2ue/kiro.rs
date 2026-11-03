#!/usr/bin/env node
// Overload probe: does kiro-rs keep dispatching, and stay healthy, when load exceeds capacity?
//
// Drives a kiro-rs on mock upstreams with an open-loop arrival rate (requests per second), so
// load keeps arriving whether or not earlier requests finished, like real traffic behind a
// gateway. The rate follows a profile (steady, ramp, spike, square wave). Each arrival can retry
// on 429 the way sub2api does: a few retries on the "same account" 500 ms apart, then "switch
// accounts" up to N times. Optionally clients honor Retry-After.
//
// Once a second it samples:
// - client side: arrivals, completions, 200s, 429s, other errors, in-flight, p50/p95 latency of
//   requests finished in that second, and how long 429s took to come back;
// - /readyz: queued requests, dispatch rate, seconds since last dispatch;
// - the mock upstream: calls in flight per account and calls in the last minute per account;
// - /api/admin/usage-writer-stats: usage writer backlog (accepted - finished).
//
// Flags per second:
// - stall: requests are queued or in flight but nothing completed upstream in the last 5 s;
// - idleWhileWaiting: requests queued or rejected while upstream slots are less than 90% used
//   (accounts below both their concurrency and RPM limits). A few slots are always between a
//   release and the next dispatch, so 100% is not the bar;
// - slowReject: p95 time for a 429 above 1 s (rejecting should be cheap);
// - usageLag: usage writer backlog above 2000.
//
// Usage: node overload-probe.mjs --base-url URL --api-key KEY --admin-key KEY --kiro-mock URL
//          --profile "steady:5:60,spike:40:30,steady:5:120" [--stream-share 0.7]
//          [--retry sub2api|none|honor] [--same-account-retries 3] [--max-switches 10]
//          [--accounts 57] [--per-account-concurrency 4] [--per-account-rpm 0]
//          [--account-limits limits.json] [--global-max 0]
//          [--sessions 0] [--body-kb 0] [--out file.json]
//
// Profile segments are `kind:rate:secs` (steady) or `ramp:from:to:secs` or
// `square:low:high:period:secs`.
//
// --upstream-plan is a JSON array with one mock patch per profile segment (or null to leave the
// mock as it is), applied when the segment starts. It models upstream incidents during a load
// shape, for example `[null, {"errorRate":1,"errorStatus":500}, {"errorRate":0}]`.
//
// --account-limits is a JSON object `{ "<access token>": { "conc": 4, "rpm": 20 } }` for pools
// whose accounts have different limits; it overrides the two per-account flags.
//
// The client caps its connections (--max-sockets, default 300) and stops early when the host
// runs short of local ports (TIME_WAIT above --max-time-wait, default 4000): a test must not
// take the machine down with it. Only TIME_WAIT sockets on ephemeral ports count: kiro-rs sends
// `Connection: close` upstream, so the mock closes every upstream call first and its TIME_WAIT
// sockets sit on the mock's own port, where they hold no local port. A second cap on all
// TIME_WAIT sockets (4x the first) still bounds kernel state.

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
const adminKey = args.adminKey || "";
const kiroMock = args.kiroMock;
const streamShare = Number(args.streamShare ?? 0.7);
const retryMode = args.retry || "sub2api";
const sameAccountRetries = Number(args.sameAccountRetries ?? 3);
const maxSwitches = Number(args.maxSwitches ?? 10);
const accounts = Number(args.accounts || 57);
const perConcurrency = Number(args.perAccountConcurrency || 4);
const perRpm = Number(args.perAccountRpm || 0);
const accountLimits = args.accountLimits ? JSON.parse((await import("node:fs")).readFileSync(args.accountLimits, "utf8")) : null;
const limitOf = (token) => accountLimits?.[token] || { conc: perConcurrency, rpm: perRpm };
// dispatchGlobalMaxConcurrentRequests caps the pool below its account slots when it is smaller.
const globalMax = Number(args.globalMax || 0);
const accountSlots = accountLimits
  ? Object.values(accountLimits).reduce((sum, limit) => sum + limit.conc, 0)
  : accounts * perConcurrency;
const capacity = globalMax > 0 ? Math.min(accountSlots, globalMax) : accountSlots;
const rpmLimited = accountLimits ? Object.values(accountLimits).some((limit) => limit.rpm > 0) : perRpm > 0;
const sessions = Number(args.sessions || 0);
const bodyKb = Number(args.bodyKb || 0);
const path = args.path || "/cc/v1/messages";
const clientTimeoutMs = Number(args.clientTimeoutMs || 300000);
// Like a gateway, the client caps its connections to kiro-rs. Without a cap the retry storm opens
// tens of thousands of sockets and exhausts the local ephemeral ports (16k on macOS), which then
// also starves kiro-rs's own connections to the mock upstream on the same loopback.
// `maxFreeSockets` defaults to 256: past that, every finished response closes its connection,
// and the retry storm churns through sockets until TIME_WAIT has used up every local port.
const maxSockets = Number(args.maxSockets || 300);
const maxTimeWait = Number(args.maxTimeWait || 4000);
const upstreamPlan = args.upstreamPlan ? JSON.parse(args.upstreamPlan) : [];
const agent = new http.Agent({ keepAlive: true, maxSockets, maxFreeSockets: maxSockets });
const padding = bodyKb > 0 ? "x".repeat(bodyKb * 1024) : "";

function parseProfile(text) {
  const segments = [];
  for (const raw of text.split(",")) {
    const parts = raw.split(":");
    const kind = parts[0];
    if (kind === "steady") segments.push({ kind, rate: Number(parts[1]), secs: Number(parts[2]) });
    else if (kind === "ramp") segments.push({ kind, from: Number(parts[1]), to: Number(parts[2]), secs: Number(parts[3]) });
    else if (kind === "square")
      segments.push({ kind, low: Number(parts[1]), high: Number(parts[2]), period: Number(parts[3]), secs: Number(parts[4]) });
    else if (kind === "spike") segments.push({ kind: "steady", rate: Number(parts[1]), secs: Number(parts[2]) });
    else throw new Error(`unknown profile segment ${raw}`);
  }
  return segments;
}

function rateAt(segment, t) {
  if (segment.kind === "steady") return segment.rate;
  if (segment.kind === "ramp") return segment.from + ((segment.to - segment.from) * t) / Math.max(1, segment.secs);
  if (segment.kind === "square") return Math.floor(t / segment.period) % 2 === 0 ? segment.low : segment.high;
  return 0;
}

const segments = parseProfile(args.profile || "steady:5:60");
const totalSecs = segments.reduce((sum, segment) => sum + segment.secs, 0);

function sendOne(marker, stream, sessionId) {
  const body = JSON.stringify({
    model: args.model || "sonnet",
    max_tokens: 64,
    stream,
    metadata: { user_id: `user_overload_probe__session_${sessionId}` },
    messages: [{ role: "user", content: `overload probe ${marker} ${padding}` }],
  });
  // Latency is measured from when the request gets a connection: with the connection cap, time
  // spent waiting for a free socket is the client's own queue, not kiro-rs.
  let started = performance.now();
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
        let tail = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          tail = (tail + chunk).slice(-2048);
        });
        res.on("end", () => {
          const streamError = stream && res.statusCode === 200 && /event: error/.test(tail);
          let kind = "";
          if (res.statusCode === 429) {
            if (/Redis 调度协调/.test(tail)) kind = "redis_unavailable";
            else if (/等待队列已满/.test(tail)) kind = "queue_full";
            else if (/排队等待超时/.test(tail)) kind = "queue_timeout";
            else kind = "other_429";
          }
          resolve({
            status: streamError ? "stream_error" : String(res.statusCode),
            kind,
            ms: performance.now() - started,
            retryAfter: Number(res.headers["retry-after"]) || null,
          });
        });
        res.on("error", () => resolve({ status: "read_error", ms: performance.now() - started }));
      },
    );
    req.on("socket", () => {
      started = performance.now();
    });
    req.setTimeout(clientTimeoutMs, () => req.destroy(new Error("client_timeout")));
    req.on("error", (error) =>
      resolve({
        status: error.message === "client_timeout" ? "client_timeout" : `conn_error:${error.code || error.message}`,
        ms: performance.now() - started,
      }),
    );
    req.end(body);
  });
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Per-second buckets keyed by second since start.
const buckets = new Map();
function bucket(sec) {
  let b = buckets.get(sec);
  if (!b) {
    b = { arrivals: 0, attempts: 0, ok: 0, rejected: 0, rejectKinds: {}, other: 0, otherKinds: {}, finalFail: 0, okMs: [], rejectMs: [] };
    buckets.set(sec, b);
  }
  return b;
}
const startedAt = Date.now();
const nowSec = () => Math.floor((Date.now() - startedAt) / 1000);
let inFlightAttempts = 0;
let openArrivals = 0;
let seq = 0;

async function arrival(n) {
  openArrivals += 1;
  bucket(nowSec()).arrivals += 1;
  const stream = Math.random() < streamShare;
  const sessionId = sessions > 0 ? `s${n % sessions}` : `a${n}`;
  let switches = 0;
  let sameRetries = 0;
  for (;;) {
    inFlightAttempts += 1;
    const result = await sendOne(n, stream, sessionId);
    inFlightAttempts -= 1;
    const b = bucket(nowSec());
    b.attempts += 1;
    if (result.status === "200") {
      b.ok += 1;
      b.okMs.push(result.ms);
      break;
    }
    if (result.status === "429") {
      b.rejected += 1;
      b.rejectMs.push(result.ms);
      b.rejectKinds[result.kind] = (b.rejectKinds[result.kind] || 0) + 1;
      if (retryMode === "none") {
        b.finalFail += 1;
        break;
      }
      if (retryMode === "honor") {
        if (switches >= maxSwitches) {
          b.finalFail += 1;
          break;
        }
        switches += 1;
        await sleep((result.retryAfter || 1) * 1000 * (1 + Math.random() * 0.5));
        continue;
      }
      // sub2api: retry the same account `sameAccountRetries` times 500 ms apart, then switch.
      if (sameRetries < sameAccountRetries) {
        sameRetries += 1;
        await sleep(500);
        continue;
      }
      if (switches < maxSwitches) {
        switches += 1;
        sameRetries = 0;
        continue;
      }
      b.finalFail += 1;
      break;
    }
    b.other += 1;
    b.otherKinds[result.status] = (b.otherKinds[result.status] || 0) + 1;
    b.finalFail += 1;
    break;
  }
  openArrivals -= 1;
}

async function getJson(url, headers = {}) {
  const started = performance.now();
  try {
    const response = await fetch(url, { headers, signal: AbortSignal.timeout(5000) });
    return { status: response.status, body: await response.json(), ms: performance.now() - started };
  } catch {
    return { status: "down", body: null, ms: performance.now() - started };
  }
}

const pct = (list, p) => {
  if (!list.length) return null;
  const sorted = [...list].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))]);
};

async function main() {
  const timeline = [];
  let stop = false;
  let lastMockOk = 0;
  const mockOkHistory = [];

  const sampler = (async () => {
    while (!stop) {
      await sleep(1000);
      const sec = nowSec() - 1;
      const [ready, mock, writer] = await Promise.all([
        getJson(`${baseUrl.origin}/readyz`),
        getJson(`${kiroMock}/__control`),
        adminKey ? getJson(`${baseUrl.origin}/api/admin/usage-writer-stats`, { "x-api-key": adminKey }) : Promise.resolve({ body: null }),
      ]);
      const perToken = mock.body?.counters?.perToken || {};
      const values = Object.values(perToken);
      const upstreamInFlight = values.reduce((sum, stats) => sum + stats.inFlight, 0);
      const neverUsed = Object.keys(accountLimits || {}).filter((token) => !perToken[token]);
      // Calls an account could take right now: free concurrency, capped by what its RPM window
      // still allows. Accounts not called yet have all their room.
      const roomOf = (limit, stats) =>
        Math.max(0, Math.min(limit.conc - stats.inFlight, limit.rpm > 0 ? limit.rpm - stats.lastMinute : Infinity));
      let withRoom = 0;
      let freeUnits = 0;
      for (const [token, stats] of Object.entries(perToken)) {
        const room = roomOf(limitOf(token), stats);
        if (room > 0) withRoom += 1;
        freeUnits += room;
      }
      if (accountLimits) {
        withRoom += neverUsed.length;
        for (const token of neverUsed) freeUnits += roomOf(limitOf(token), { inFlight: 0, lastMinute: 0 });
      } else {
        const unseen = Math.max(0, accounts - values.length);
        withRoom += unseen;
        freeUnits += unseen * roomOf(limitOf(""), { inFlight: 0, lastMinute: 0 });
      }
      if (globalMax > 0) freeUnits = Math.min(freeUnits, Math.max(0, globalMax - upstreamInFlight));
      const mockOk = mock.body?.counters?.ok ?? lastMockOk;
      const upstreamDone = Math.max(0, mockOk - lastMockOk);
      lastMockOk = mockOk;
      mockOkHistory.push(upstreamDone);
      const last5 = mockOkHistory.slice(-5).reduce((sum, v) => sum + v, 0);
      const b = bucket(sec);
      const queued = ready.body?.dispatch?.queuedRequests ?? null;
      const w = writer.body || {};
      const usageBacklog = typeof w.writerAccepted === "number" ? w.writerAccepted - (w.writerFinished || 0) : null;
      const waiting = (queued || 0) > 0 || b.rejected > 0;
      const rejectP95 = pct(b.rejectMs, 95);
      timeline.push({
        t: sec,
        targetRate: Math.round(currentRate * 10) / 10,
        arrivals: b.arrivals,
        attempts: b.attempts,
        ok: b.ok,
        rejected: b.rejected,
        rejectKinds: b.rejectKinds,
        other: b.other,
        otherKinds: b.otherKinds,
        finalFail: b.finalFail,
        okP50: pct(b.okMs, 50),
        okP95: pct(b.okMs, 95),
        rejectP50: pct(b.rejectMs, 50),
        rejectP95,
        clientInFlight: inFlightAttempts,
        openArrivals,
        ready: ready.status,
        readyMs: Math.round(ready.ms),
        // Which readiness checks failed when /readyz was not ready.
        readyFailed:
          ready.status === 200 || !ready.body?.checks
            ? null
            : Object.entries(ready.body.checks)
                .filter(([, ok]) => !ok)
                .map(([name]) => name),
        queued,
        runtimeLagMs: ready.body?.runtimeLagMs ?? null,
        pendingReleases: ready.body?.dispatch?.pendingRedisReleases ?? null,
        dispatchRate: ready.body?.dispatch?.dispatchRatePerSec ?? null,
        secsSinceLastDispatch: ready.body?.dispatch?.secsSinceLastDispatch ?? null,
        upstreamInFlight,
        upstreamDone,
        accountsWithRoom: withRoom,
        freeUnits,
        usageBacklog,
        usageDropped: w.droppedPersistRecords ?? null,
        redisUsageBacklog:
          typeof w.redisWriterAccepted === "number" ? w.redisWriterAccepted - (w.redisWriterFinished || 0) : null,
        // Stalled: requests are waiting in kiro-rs (queued, or readyz itself is not answering
        // while clients wait beyond what is upstream) and no upstream call started for 5 s.
        // Long streams with an empty queue are not a stall.
        stall:
          mockOkHistory.length >= 5 &&
          last5 === 0 &&
          ((queued || 0) > 0 || (ready.status !== 200 && inFlightAttempts > upstreamInFlight)),
        slotUse: Math.round((upstreamInFlight / Math.max(1, capacity)) * 1000) / 1000,
        idleWhileWaiting:
          waiting &&
          withRoom > 0 &&
          upstreamInFlight < 0.9 * capacity &&
          freeUnits > 0,
        slowReject: rejectP95 !== null && rejectP95 > 1000,
        usageLag: usageBacklog !== null && usageBacklog > 2000,
      });
    }
  })();

  let currentRate = 0;
  const arrivals = [];
  let elapsed = 0;
  let abortedReason = null;
  const { execFileSync } = await import("node:child_process");
  // [TIME_WAIT holding an ephemeral port, all TIME_WAIT].
  const timeWaitCount = () => {
    try {
      const lines = execFileSync("netstat", ["-an", "-p", "tcp"], { encoding: "utf8" })
        .split("\n")
        .filter((line) => line.includes("TIME_WAIT"));
      const ephemeral = lines.filter((line) => {
        const port = Number(line.trim().split(/\s+/)[3]?.split(".").pop());
        return port >= 49152;
      }).length;
      return [ephemeral, lines.length];
    } catch {
      return [0, 0];
    }
  };
  let lastPortCheck = 0;
  // When each segment actually started, in sample seconds. On a loaded host the sampler falls
  // behind wall time, so phases cut by nominal durations put a phase's first seconds (an
  // upstream outage, say) into the previous one.
  const segmentStartSecs = [];
  for (const [segmentIndex, segment] of segments.entries()) {
    if (abortedReason) break;
    segmentStartSecs.push(nowSec());
    const patch = upstreamPlan[segmentIndex];
    if (patch) {
      await fetch(`${kiroMock}/__control`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(patch),
      }).catch(() => {});
    }
    const segmentStart = Date.now();
    let sent = 0;
    while (Date.now() - segmentStart < segment.secs * 1000) {
      if (Date.now() - lastPortCheck > 5000) {
        lastPortCheck = Date.now();
        const [timeWait, allTimeWait] = timeWaitCount();
        if (timeWait > maxTimeWait || allTimeWait > maxTimeWait * 4) {
          abortedReason = `TIME_WAIT ${timeWait} on ephemeral ports (${allTimeWait} in all) over ${maxTimeWait}/${maxTimeWait * 4}, stopping to protect the host`;
          break;
        }
      }
      const t = (Date.now() - segmentStart) / 1000;
      currentRate = rateAt(segment, t);
      // Open loop: send whatever the integral of the rate says should have gone out by now.
      let due = 0;
      const steps = 20;
      for (let i = 0; i < steps; i += 1) due += rateAt(segment, (t * i) / steps) * (t / steps);
      const target = Math.floor(due);
      while (sent < target) {
        arrivals.push(arrival(seq++));
        sent += 1;
      }
      await sleep(20);
    }
    elapsed += segment.secs;
  }
  currentRate = 0;
  // Drain: let open arrivals finish (bounded), keep sampling to see recovery.
  const drainStartSec = nowSec();
  const drainUntil = Date.now() + Number(args.drainSecs || 90) * 1000;
  while (openArrivals > 0 && Date.now() < drainUntil) await sleep(500);
  await sleep(3000);
  stop = true;
  await sampler;

  const phaseOf = (t) => {
    if (t >= drainStartSec) return segments.length;
    for (let i = segmentStartSecs.length - 1; i >= 0; i -= 1) {
      if (t >= segmentStartSecs[i]) return i;
    }
    return 0;
  };
  const phases = [...segments.map((segment, index) => ({ index, ...segment })), { index: segments.length, kind: "drain" }].map(
    (segment) => {
      const samples = timeline.filter((sample) => phaseOf(sample.t) === segment.index);
      const sum = (key) => samples.reduce((acc, sample) => acc + (sample[key] || 0), 0);
      const kinds = {};
      for (const sample of samples) for (const [k, v] of Object.entries(sample.rejectKinds)) kinds[k] = (kinds[k] || 0) + v;
      const otherKinds = {};
      for (const sample of samples) for (const [k, v] of Object.entries(sample.otherKinds)) otherKinds[k] = (otherKinds[k] || 0) + v;
      const okRates = samples.map((sample) => sample.ok);
      return {
        segment: segment.kind === "drain" ? "drain" : Object.values(segment).slice(1).join(":"),
        secs: samples.length,
        arrivals: sum("arrivals"),
        attempts: sum("attempts"),
        ok: sum("ok"),
        rejected: sum("rejected"),
        rejectKinds: kinds,
        other: sum("other"),
        otherKinds,
        finalFail: sum("finalFail"),
        okPerSecMin: okRates.length ? Math.min(...okRates.slice(Math.min(5, okRates.length - 1))) : null,
        okPerSecAvg: samples.length ? Math.round((sum("ok") / samples.length) * 10) / 10 : null,
        upstreamInFlightMax: Math.max(0, ...samples.map((sample) => sample.upstreamInFlight)),
        // Average slot use over the seconds in which requests were waiting (queued or rejected).
        slotUseWhileWaiting: (() => {
          const waiting = samples.filter((sample) => (sample.queued || 0) > 0 || sample.rejected > 0);
          if (!waiting.length) return null;
          return Math.round((waiting.reduce((sum, sample) => sum + sample.slotUse, 0) / waiting.length) * 1000) / 1000;
        })(),
        // Average share of the pool's slots that were free to dispatch (concurrency and RPM both
        // allowing) over the seconds in which requests were waiting: invariant A wants ~0.
        freeWhileWaiting: (() => {
          const waiting = samples.filter((sample) => (sample.queued || 0) > 0 || sample.rejected > 0);
          if (!waiting.length) return null;
          return Math.round((waiting.reduce((sum, sample) => sum + sample.freeUnits, 0) / waiting.length / Math.max(1, capacity)) * 1000) / 1000;
        })(),
        queuedMax: Math.max(0, ...samples.map((sample) => sample.queued || 0)),
        rejectP95Max: Math.max(0, ...samples.map((sample) => sample.rejectP95 || 0)),
        readyMsMax: Math.max(0, ...samples.map((sample) => sample.readyMs || 0)),
        runtimeLagMsMax: Math.max(0, ...samples.map((sample) => sample.runtimeLagMs || 0)),
        usageBacklogMax: Math.max(0, ...samples.map((sample) => sample.usageBacklog || 0)),
        stallSecs: samples.filter((sample) => sample.stall).length,
        idleWhileWaitingSecs: samples.filter((sample) => sample.idleWhileWaiting).length,
        slowRejectSecs: samples.filter((sample) => sample.slowReject).length,
        usageLagSecs: samples.filter((sample) => sample.usageLag).length,
        notReadySecs: samples.filter((sample) => sample.ready !== 200).length,
      };
    },
  );
  const finalMock = await getJson(`${kiroMock}/__control`);
  const perToken = finalMock.body?.counters?.perToken || {};
  // kiro-rs's own count of calls it let through per account, to tell a real RPM overrun from
  // calls that only arrived at the mock bunched together (arrival delay on a loaded host).
  const tokenIds = Object.keys(perToken)
    .map((token) => token.replace("mock-access-", ""))
    .filter((id) => /^[0-9]+$/.test(id));
  const runtime =
    adminKey && tokenIds.length
      ? await getJson(`${baseUrl.origin}/api/admin/credentials/runtime?ids=${tokenIds.join(",")}`, {
          "x-api-key": adminKey,
        })
      : { body: null };
  const dispatchedByToken = {};
  for (const item of runtime.body?.items || []) {
    dispatchedByToken[`mock-access-${item.id}`] = item.schedulerSelectionCount ?? null;
  }
  const maxInFlights = Object.values(perToken).map((stats) => stats.maxInFlight);
  const overConcurrency = Object.entries(perToken).filter(([token, stats]) => stats.maxInFlight > limitOf(token).conc);
  const overRpm = Object.entries(perToken).filter(([token, stats]) => limitOf(token).rpm > 0 && stats.maxPerMinute > limitOf(token).rpm);
  const report = {
    abortedReason,
    accounts,
    perConcurrency,
    perRpm,
    segmentStartSecs,
    drainStartSec,
    capacity,
    accountSlots,
    globalMax,
    rpmLimited,
    retryMode,
    profile: args.profile,
    phases,
    accountsUsed: Object.keys(perToken).length,
    accountMaxInFlight: Math.max(0, ...maxInFlights),
    accountsOverConcurrency: overConcurrency.length,
    accountsOverConcurrencyDetail: overConcurrency
      .slice(0, 5)
      .map(([token, stats]) => ({ token, maxInFlight: stats.maxInFlight, limit: limitOf(token).conc })),
    // Event-loop lag of the mock over the run: its arrival and close times trail kiro-rs by up
    // to this much, so RPM and concurrency peaks within it are measurement, not dispatch.
    mockLoopLagMaxMs: finalMock.body?.counters?.loopLagMaxMs ?? null,
    accountsOverRpm: overRpm.length,
    accountsOverRpmDetail: overRpm.slice(0, 5).map(([token, stats]) => ({
      token,
      maxPerMinute: stats.maxPerMinute,
      rpm: limitOf(token).rpm,
      upstreamCalls: stats.calls,
      dispatched: dispatchedByToken[token] ?? null,
    })),
    unfinishedArrivals: openArrivals,
    timeline,
  };
  if (args.out) {
    const fs = await import("node:fs");
    fs.writeFileSync(args.out, JSON.stringify(report, null, 2));
  }
  for (const phase of phases) {
    console.log(
      `[${phase.segment}] secs=${phase.secs} arrivals=${phase.arrivals} attempts=${phase.attempts} ok=${phase.ok} okPerSec(avg/min)=${phase.okPerSecAvg}/${phase.okPerSecMin} rejected=${phase.rejected} ${JSON.stringify(phase.rejectKinds)} other=${phase.other} ${JSON.stringify(phase.otherKinds)} finalFail=${phase.finalFail} upstreamMax=${phase.upstreamInFlightMax} slotUseWhileWaiting=${phase.slotUseWhileWaiting} freeWhileWaiting=${phase.freeWhileWaiting} queuedMax=${phase.queuedMax} rejectP95Max=${phase.rejectP95Max} readyMsMax=${phase.readyMsMax} lagMsMax=${phase.runtimeLagMsMax} usageBacklogMax=${phase.usageBacklogMax} stall=${phase.stallSecs} idleWhileWaiting=${phase.idleWhileWaitingSecs} slowReject=${phase.slowRejectSecs} usageLag=${phase.usageLagSecs} notReady=${phase.notReadySecs}`,
    );
  }
  if (abortedReason) console.log(`ABORTED: ${abortedReason}`);
  console.log(
    `accountsUsed=${report.accountsUsed}/${accounts} capacity=${capacity} accountMaxInFlight=${report.accountMaxInFlight} overConcurrency=${report.accountsOverConcurrency} overRpm=${report.accountsOverRpm} unfinished=${report.unfinishedArrivals}`,
  );
  agent.destroy();
  process.exit(0);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
