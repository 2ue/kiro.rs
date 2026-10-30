#!/usr/bin/env node
// Capacity probe: do requests queue or get turned away while accounts still have room?
//
// Ramps concurrency step by step against a kiro-rs on mock upstreams. Once a second it samples
// the dispatch queue (/readyz) and, from the mock upstream, how many calls each account has in
// flight right now and how many it took in the last minute. A second in which requests were
// queued or rejected while some account was below both its concurrency and its RPM limit is an
// "idle while waiting" second: capacity was there but was not used.
//
// Usage: node capacity-probe.mjs --base-url URL --api-key KEY --kiro-mock URL
//          --accounts 100 --per-account-concurrency 3 --per-account-rpm 20
//          --steps 60,90,120,150,200,300 --step-secs 20 [--stream-share 0.5]
//          [--sessions 40] [--out file.json]

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
const accounts = Number(args.accounts || 100);
const perConcurrency = Number(args.perAccountConcurrency || 3);
const perRpm = Number(args.perAccountRpm || 20);
const steps = String(args.steps || "60,90,120,150,200,300").split(",").map(Number);
const stepSecs = Number(args.stepSecs || 20);
const streamShare = Number(args.streamShare ?? 0.5);
// Number of distinct sticky sessions the clients rotate through; 0 means a new session per call.
const sessions = Number(args.sessions || 0);
const path = args.path || "/cc/v1/messages";
const agent = new http.Agent({ keepAlive: true, maxSockets: 4096 });

function sendOne(marker, stream, sessionId) {
  const body = JSON.stringify({
    model: args.model || "sonnet",
    max_tokens: 64,
    stream,
    metadata: { user_id: `user_capacity_probe__session_${sessionId}` },
    messages: [{ role: "user", content: `capacity probe ${marker}` }],
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
        let tail = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => {
          tail = (tail + chunk).slice(-2048);
        });
        res.on("end", () => {
          const streamError = stream && res.statusCode === 200 && /event: error/.test(tail);
          resolve({ status: streamError ? "stream_error" : String(res.statusCode), ms: performance.now() - started, at: Date.now() });
        });
        res.on("error", () => resolve({ status: "read_error", ms: performance.now() - started, at: Date.now() }));
      },
    );
    req.setTimeout(180000, () => req.destroy(new Error("client_timeout")));
    req.on("error", (error) =>
      resolve({ status: error.message === "client_timeout" ? "client_timeout" : "conn_error", ms: performance.now() - started, at: Date.now() }),
    );
    req.end(body);
  });
}

async function getJson(url) {
  try {
    const response = await fetch(url);
    return { status: response.status, body: await response.json() };
  } catch {
    return { status: "down", body: null };
  }
}

async function main() {
  const results = [];
  const timeline = [];
  let target = 0;
  let active = 0;
  let seq = 0;
  let stop = false;
  const startedAt = Date.now();

  const worker = async (id) => {
    active += 1;
    while (!stop && id < target) {
      const n = seq++;
      const stream = Math.random() < streamShare;
      const sessionId = sessions > 0 ? `s${n % sessions}` : `w${id}-${n}`;
      const result = await sendOne(n, stream, sessionId);
      result.target = target;
      results.push(result);
    }
    active -= 1;
  };

  const sampler = (async () => {
    while (!stop) {
      await new Promise((resolve) => setTimeout(resolve, 1000));
      const [ready, mock] = await Promise.all([getJson(`${baseUrl.origin}/readyz`), getJson(`${kiroMock}/__control`)]);
      const perToken = mock.body?.counters?.perToken || {};
      const values = Object.values(perToken);
      const inFlightTotal = values.reduce((sum, stats) => sum + stats.inFlight, 0);
      // An account still has room when it is below both its concurrency and its RPM limit.
      const withRoom = values.filter((stats) => stats.inFlight < perConcurrency && stats.lastMinute < perRpm).length;
      const neverUsed = accounts - values.length;
      const zeroInFlightWithRoom = values.filter((stats) => stats.inFlight === 0 && stats.lastMinute < perRpm).length + neverUsed;
      const lastSecond = results.filter((result) => result.at > Date.now() - 1000);
      const rejected = lastSecond.filter((result) => result.status === "429").length;
      const queued = ready.body?.dispatch?.queuedRequests ?? null;
      const waiting = (queued || 0) > 0 || rejected > 0;
      timeline.push({
        t: Math.round((Date.now() - startedAt) / 1000),
        target,
        ready: ready.status,
        progressing: ready.body?.dispatch?.progressing ?? null,
        queued,
        rejected,
        ok: lastSecond.filter((result) => result.status === "200").length,
        other: lastSecond.filter((result) => result.status !== "200" && result.status !== "429").length,
        inFlightTotal,
        accountsSeen: values.length,
        accountsWithRoom: withRoom + neverUsed,
        zeroInFlightWithRoom,
        idleWhileWaiting: waiting && withRoom + neverUsed > 0,
      });
    }
  })();

  const workers = [];
  for (const step of steps) {
    target = step;
    for (let id = workers.length; id < step; id += 1) workers.push(worker(id));
    await new Promise((resolve) => setTimeout(resolve, stepSecs * 1000));
  }
  stop = true;
  await Promise.all(workers);
  await sampler;

  const capacity = Math.min(accounts * perConcurrency, Infinity);
  const summary = steps.map((step) => {
    const inStep = results.filter((result) => result.target === step);
    const samples = timeline.filter((sample) => sample.target === step);
    const byStatus = {};
    for (const result of inStep) byStatus[result.status] = (byStatus[result.status] || 0) + 1;
    const ok = inStep.filter((result) => result.status === "200").map((result) => result.ms).sort((a, b) => a - b);
    const pct = (list, p) => (list.length ? Math.round(list[Math.min(list.length - 1, Math.floor((p / 100) * list.length))]) : null);
    const idle = samples.filter((sample) => sample.idleWhileWaiting);
    return {
      concurrency: step,
      total: inStep.length,
      byStatus,
      okP50: pct(ok, 50),
      okP95: pct(ok, 95),
      maxInFlightUpstream: Math.max(0, ...samples.map((sample) => sample.inFlightTotal)),
      maxQueued: Math.max(0, ...samples.map((sample) => sample.queued || 0)),
      secondsWaiting: samples.filter((sample) => (sample.queued || 0) > 0 || sample.rejected > 0).length,
      idleWhileWaitingSeconds: idle.length,
      worstIdleWhileWaiting: idle.sort((a, b) => b.zeroInFlightWithRoom - a.zeroInFlightWithRoom)[0] || null,
      stalledSeconds: samples.filter((sample) => sample.progressing === false || sample.ready === 503).length,
    };
  });
  const finalMock = await getJson(`${kiroMock}/__control`);
  const perToken = finalMock.body?.counters?.perToken || {};
  const maxInFlights = Object.values(perToken).map((stats) => stats.maxInFlight);
  const maxPerMinute = Object.values(perToken).map((stats) => stats.maxPerMinute);
  const report = {
    accounts,
    perConcurrency,
    perRpm,
    capacityConcurrency: capacity,
    capacityPerMinute: accounts * perRpm,
    steps: summary,
    accountsUsed: Object.keys(perToken).length,
    accountMaxInFlight: { max: Math.max(0, ...maxInFlights), overLimit: maxInFlights.filter((v) => v > perConcurrency).length },
    accountMaxPerMinute: { max: Math.max(0, ...maxPerMinute), overLimit: maxPerMinute.filter((v) => v > perRpm).length },
    timeline,
  };
  if (args.out) {
    const fs = await import("node:fs");
    fs.writeFileSync(args.out, JSON.stringify(report, null, 2));
  }
  for (const step of summary) {
    console.log(
      `c=${step.concurrency} total=${step.total} status=${JSON.stringify(step.byStatus)} okP50=${step.okP50} okP95=${step.okP95} upstreamInFlightMax=${step.maxInFlightUpstream} queuedMax=${step.maxQueued} waitingSecs=${step.secondsWaiting} idleWhileWaitingSecs=${step.idleWhileWaitingSeconds} stalledSecs=${step.stalledSeconds}${step.worstIdleWhileWaiting ? ` worst={queued:${step.worstIdleWhileWaiting.queued},rejected:${step.worstIdleWhileWaiting.rejected},withRoom:${step.worstIdleWhileWaiting.accountsWithRoom},zeroInFlight:${step.worstIdleWhileWaiting.zeroInFlightWithRoom}}` : ""}`,
    );
  }
  console.log(
    `accounts used=${report.accountsUsed}/${accounts} account maxInFlight=${report.accountMaxInFlight.max} (over limit ${report.accountMaxInFlight.overLimit}) maxPerMinute=${report.accountMaxPerMinute.max} (over limit ${report.accountMaxPerMinute.overLimit})`,
  );
  agent.destroy();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
