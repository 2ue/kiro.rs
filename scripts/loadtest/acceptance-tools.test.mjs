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
