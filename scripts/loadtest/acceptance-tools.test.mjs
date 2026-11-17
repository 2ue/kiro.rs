import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";

async function run(command, argv) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, argv, { stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    child.on("error", reject);
    child.on("close", (code) => resolve({ code, output }));
  });
}

test("probe retries 529 and maps masked accounts by name; drain verdict rejects remaining leases", async () => {
  const directory = mkdtempSync(join(tmpdir(), "acceptance-tools-"));
  let attempts = 0;
  const server = http.createServer((req, res) => {
    res.setHeader("content-type", "application/json");
    if (req.url === "/cc/v1/messages") {
      req.resume();
      req.on("end", () => {
        attempts += 1;
        res.statusCode = attempts === 1 ? 529 : 200;
        res.end(JSON.stringify(attempts === 1 ? { error: { type: "overloaded_error" } } : { type: "message" }));
      });
      return;
    }
    if (req.url === "/api/admin/accounts/status") {
      res.end(JSON.stringify({ accounts: [{ account: { name: "mock-access-7001", apiKey: "***" }, dispatchedTotal: 17 }],
        summary: { inFlightTotal: 0, queuedRequests: 0 } }));
    } else if (req.url === "/__control") {
      res.end(JSON.stringify({ counters: { ok: 2, perToken: { "mock-access-7001": { inFlight: 0, maxInFlight: 1, lastMinute: 2, calls: 2 } } } }));
    } else {
      res.end(JSON.stringify({ dispatch: { queuedRequests: 0, dispatchRatePerSec: 1, progressing: true } }));
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}`;
  try {
    const reportPath = join(directory, "steady_ok.json");
    const probe = await run(process.execPath, ["scripts/loadtest/overload-probe.mjs", "--base-url", url,
      "--api-key", "test", "--admin-key", "admin", "--pool-mock", url, "--profile", "steady:2:1.1",
      "--stream-share", "0", "--retry", "sub2api", "--accounts", "1", "--out", reportPath]);
    assert.equal(probe.code, 0, probe.output);
    const report = JSON.parse(readFileSync(reportPath));
    assert.equal(report.dispatchedByToken["mock-access-7001"], 17);
    assert.equal(report.phases.reduce((sum, phase) => sum + phase.rejected, 0), 1);
    assert.equal(report.phases.reduce((sum, phase) => sum + (phase.rejectKinds.overloaded_error || 0), 0), 1);
    assert.equal(report.unfinishedArrivals, 0);
    assert.ok(attempts >= 3);
    const passing = await run("python3", ["scripts/loadtest/overload-verdict.py", directory]);
    assert.equal(passing.code, 0, passing.output);
    report.accountsSummary.inFlightTotal = 1;
    writeFileSync(reportPath, JSON.stringify(report));
    const failing = await run("python3", ["scripts/loadtest/overload-verdict.py", directory]);
    assert.equal(failing.code, 1);
    assert.match(failing.output, /drain left inFlightTotal=1/);
  } finally {
    await new Promise((resolve) => server.close(resolve));
    rmSync(directory, { recursive: true, force: true });
  }
});

test("process sample: liveness comes from signal-0, a failed ps/lsof only leaves rss/fd unknown", async () => {
  const { processSample, processAlive, resourceSummary } = await import("./lib/process-sample.mjs");
  assert.equal(processAlive(process.pid), true);
  assert.equal(processAlive(999_999), false);
  const self = processSample(process.pid);
  assert.equal(self.alive, true);
  assert.ok(self.rssMb > 0 && self.fd > 0);
  const dead = processSample(999_999);
  assert.deepEqual(dead, { rssMb: null, fd: null, fdByKind: null, alive: false });
  // An alive sample whose ps/lsof failed must not be counted as "exited" and must not poison the summary.
  const unsampled = { rssMb: null, fd: null, fdByKind: null, alive: true, sampled: false };
  const summary = resourceSummary([self, unsampled, self]);
  assert.equal(summary.start.fd, self.fd);
  assert.equal(summary.end.alive, true);
  assert.equal([self, unsampled, self].some((s) => s && !s.alive), false);
});

test("fd leak verdict judges per kind: pooled idle sockets under their caps are not a leak", async () => {
  const { classifyLsof, fdLeakProblems } = await import("./lib/process-sample.mjs");
  const lsof = (rows) => ["COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME", ...rows].join("\n");
  const listen = "claude-po 1 u 3u IPv4 0x1 0t0 TCP 127.0.0.1:19381 (LISTEN)";
  const downstream = (n) => Array.from({ length: n }, (_, i) => `claude-po 1 u ${10 + i}u IPv4 0x1 0t0 TCP 127.0.0.1:19381->127.0.0.1:${50000 + i} (ESTABLISHED)`);
  const upstream = (n) => Array.from({ length: n }, (_, i) => `claude-po 1 u ${100 + i}u IPv4 0x1 0t0 TCP 127.0.0.1:${51000 + i}->127.0.0.1:39181 (ESTABLISHED)`);
  const pg = (n) => Array.from({ length: n }, (_, i) => `claude-po 1 u ${200 + i}u IPv6 0x1 0t0 TCP [::1]:${52000 + i}->[::1]:25432 (ESTABLISHED)`);
  const redis = (n) => Array.from({ length: n }, (_, i) => `claude-po 1 u ${300 + i}u IPv4 0x1 0t0 TCP 127.0.0.1:${53000 + i}->127.0.0.1:26379 (ESTABLISHED)`);
  const other = (n) => Array.from({ length: n }, (_, i) => i % 2 ? `claude-po 1 u ${400 + i}u KQUEUE count=0, state=0xa` : `claude-po 1 u ${400 + i}u unix 0x1 0t0 ->0x2`);
  const ports = { downstreamPort: "19381", upstreamPort: "39181" };

  const start = classifyLsof(lsof([listen, ...upstream(6), ...pg(9), ...redis(4), ...other(22)]), ports);
  assert.deepEqual(start, { total: 42, downstream: 1, upstream: 6, db: 13, other: 22 });
  // The situation that used to fail "FD did not recover": the pools warmed up and stayed idle.
  const warmPools = classifyLsof(lsof([listen, ...upstream(24), ...pg(13), ...redis(4), ...other(22)]), ports);
  assert.equal(warmPools.total, 64);
  assert.deepEqual(fdLeakProblems({ fdByKind: start }, { fdByKind: warmPools }, { upstreamCap: 40, dbCap: 24 }), []);
  // Real leaks are still caught, each on its own invariant.
  const leakedClients = classifyLsof(lsof([listen, ...downstream(5), ...upstream(6), ...pg(9), ...redis(4), ...other(22)]), ports);
  assert.match(fdLeakProblems({ fdByKind: start }, { fdByKind: leakedClients }, { upstreamCap: 40, dbCap: 24 }).join(";"), /downstream sockets not released 1 -> 6/);
  const leakedUpstream = classifyLsof(lsof([listen, ...upstream(41), ...pg(9), ...redis(4), ...other(22)]), ports);
  assert.match(fdLeakProblems({ fdByKind: start }, { fdByKind: leakedUpstream }, { upstreamCap: 40, dbCap: 24 }).join(";"), /upstream sockets 41 exceed slot cap 40/);
  const leakedDb = classifyLsof(lsof([listen, ...upstream(6), ...pg(21), ...redis(4), ...other(22)]), ports);
  assert.match(fdLeakProblems({ fdByKind: start }, { fdByKind: leakedDb }, { upstreamCap: 40, dbCap: 24 }).join(";"), /database sockets 25 exceed pool cap 24/);
  const leakedHandles = classifyLsof(lsof([listen, ...upstream(6), ...pg(9), ...redis(4), ...other(33)]), ports);
  assert.match(fdLeakProblems({ fdByKind: start }, { fdByKind: leakedHandles }, { upstreamCap: 40, dbCap: 24 }).join(";"), /non-socket FDs grew 22 -> 33/);
  // Without a breakdown the verdict cannot be trusted and says so.
  assert.deepEqual(fdLeakProblems({ fdByKind: null }, { fdByKind: start }, {}), ["FD breakdown unavailable"]);
});
