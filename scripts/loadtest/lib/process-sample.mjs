import { execFileSync } from "node:child_process";

// Liveness comes from a signal-0 probe, not from `ps`/`lsof` succeeding: on a loaded host those
// commands can time out or be killed while the service keeps running, and that must not be
// reported as "service process exited". A failed `ps`/`lsof` only leaves RSS/FD unknown.
export function processAlive(pid) {
  if (!pid) return false;
  try {
    process.kill(Number(pid), 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

export function processSample(pid) {
  if (!pid) return null;
  const alive = processAlive(pid);
  if (!alive) return { rssMb: null, fd: null, alive: false };
  let rssMb = null;
  let fd = null;
  try {
    const rssKb = Number(execFileSync("ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8", timeout: 5000 }).trim());
    if (Number.isFinite(rssKb) && rssKb > 0) rssMb = rssKb / 1024;
  } catch {}
  try {
    const lines = execFileSync("lsof", ["-nP", "-p", String(pid)], { encoding: "utf8", timeout: 10000 }).trim().split("\n");
    fd = Math.max(0, lines.length - 1);
  } catch {}
  return { rssMb, fd, alive: true, sampled: rssMb !== null && fd !== null };
}

export function resourceSummary(samples) {
  const valid = samples.filter((sample) => sample && sample.alive && sample.rssMb !== null && sample.fd !== null);
  if (!valid.length) return null;
  return {
    start: valid[0],
    peak: { rssMb: Math.max(...valid.map((s) => s.rssMb ?? 0)), fd: Math.max(...valid.map((s) => s.fd ?? 0)) },
    end: valid.at(-1),
  };
}
